pub mod oql_docs;

mod params;
use params::{EdgeIndex, ObjectIndex, SrcObjectIndex};

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use rmcp::{
    ErrorData as McpError, ServerHandler, ServiceExt,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerConfig},
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

/// Parameters for `load_dump`.
///
/// CACHE CAVEAT: the on-disk analysis cache is keyed ONLY by dump path, not
/// by these options. If this exact dump was already loaded (by you or the
/// CLI) with different options, the STALE cached report is served as-is —
/// these flags silently have no effect until the cache is cleared
/// (`hprof-analyzer heap cache-clear <dump>` from a shell, or delete the
/// `<dump>.hprof-cache/` directory). There is currently no way to force a
/// fresh analysis from MCP alone; ask the user to clear the cache if you
/// suspect this is happening (e.g. a section you expect to be populated,
/// like "collections", comes back empty even though you set
/// collections=true here).
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
    /// Enable collection-waste analysis (fill ratios, over-capacity
    /// collections, map load factors) — the MCP equivalent of the CLI's
    /// `--collections` flag. Adds ~300 MB peak RSS during analysis. Off by
    /// default; set true for a deeper first pass instead of reloading later.
    #[serde(default)]
    pub collections: bool,
    /// Enable duplicate-String/array detection and ThreadLocal leak
    /// analysis — the MCP equivalent of `--find-duplicates`. Adds a few
    /// extra heap-file scans. Off by default.
    #[serde(default)]
    pub find_duplicates: bool,
    /// Shortcut for collections=true AND find_duplicates=true — the MCP
    /// equivalent of the CLI's `--full-analysis` flag. If either this or
    /// the individual flags above are true, the corresponding analysis runs.
    #[serde(default)]
    pub full_analysis: bool,
    /// Ad hoc class categories, e.g. `["application=com.mycompany."]` — the
    /// MCP equivalent of the CLI's repeatable `--category NAME=PREFIX`
    /// flag. Classified BEFORE the built-in `framework`/`jdk` categories;
    /// unmatched classes fall through to `"other"`. See the `categories`
    /// report section and the `[[category]]` README docs for the full
    /// prefix/first-match-wins semantics.
    #[serde(default)]
    pub categories: Vec<String>,
    /// D1: accept a truncated plain .hprof file instead of returning an error.
    /// By default load_dump refuses a plain (non-gzip/zip) dump that ended
    /// mid-record; set force=true to load and analyze the partial data anyway
    /// (the ⚠ TRUNCATED warning is still included in the response).
    /// Gzip/zip/tgz inputs are always lenient and ignore this flag.
    #[serde(default)]
    pub force: bool,
}

/// Parse `["name=prefix", ...]` entries into `CategoryDef`s, the same
/// "NAME=PREFIX" syntax the CLI's `--category` flag accepts (see
/// `resolve_categories` in main.rs — kept as a separate implementation
/// rather than a shared helper since the error type differs: this returns
/// a plain `String` for the caller to wrap in `McpError::invalid_params`,
/// while the CLI calls `fail()`/exits directly).
fn parse_category_specs(specs: &[String]) -> Result<Vec<crate::classify::CategoryDef>, String> {
    let mut out = Vec::with_capacity(specs.len());
    for spec in specs {
        let Some((name, prefix)) = spec.split_once('=') else {
            return Err(format!(
                "invalid categories entry {spec:?}: expected \"NAME=PREFIX\" \
                 (e.g. \"application=com.mycompany.\")"
            ));
        };
        if name.is_empty() || prefix.is_empty() {
            return Err(format!(
                "invalid categories entry {spec:?}: NAME and PREFIX must both be non-empty"
            ));
        }
        out.push(crate::classify::CategoryDef::prefix(name, &[prefix]));
    }
    Ok(out)
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
    ///   "leaks"    — leak suspects with root paths, dominated objects, dominator tree (capped at depth 3, 20 children/node)
    ///   "top"      — LARGE: full biggest-objects + biggest-classes lists; prefer "top-objects" or "top-classes"
    ///   "threads"  — per-thread retained sizes + stack traces (sorted by retained desc, limited by limit param)
    ///   "overview" — heap totals, object count, identifier size
    ///
    /// ANALYSIS SECTIONS:
    ///   "triage", "waste", "indicators", "retainers", "arrays", "collections",
    ///   "references", "dominators", "components", "alloc_sites", "thread_locals",
    ///   "framework", "field_stats", "categories"
    ///
    /// QUICK-ACCESS SECTIONS (synthesized from existing data, no new model fields):
    ///   "gc-roots"       — GC root counts + retained bytes by root type
    ///   "classloaders"   — classloader count + loader_rollup + duplicate_classes
    ///   "strings"        — string/char[] count/shallow + duplicate waste stats
    ///   "packages"       — biggest_packages tree (which package dominates memory)
    ///   "culprits"       — primary_culprits: top-N synthesized likely culprit objects
    ///   "self-referential" — classes with many instances of themselves (list/trie node leaks)
    ///   "static-collections" — collection classes with instances/shallow/retained/ratio; high ratio = accumulation
    ///   "resources"      — file/stream/JDBC handle classes by instance count; high count = unclosed leak
    ///
    /// Default "triage" (severity-tagged signals — the fastest diagnosis).
    /// "all" (everything merged) is very large; avoid in LLM workflows.
    pub section: Option<String>,
    /// Max rows for "top-objects" and "threads" (default 20, max 100).
    /// For "top-classes" default is 10 (holders data makes each row large).
    /// Also limits rows for "retainers", "waste" (waste_breakdown rows),
    /// and "alloc_sites" (alloc site rows).
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct GetHistogramParams {
    /// Number of top classes to return sorted by retained size (default 50,
    /// server-capped at 500; the effective limit is echoed in the response).
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
    ///   "thread-pool-waiters"     — threads with retained size, sorted desc (*)
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
    ///   "xml-dom-documents"       — accumulated Xerces DOM Document objects (*)
    ///   "fjp-task-queue"          — ForkJoinTask subtypes accumulating
    ///   "classloader-instances"   — live ClassLoader instances with retained sizes (*)
    ///   "big-decimal-instances"   — BigDecimal accumulation by type
    ///   "http-sessions"           — HTTP Session objects by retained size (*)
    ///   "static-collections"      — Collection subclasses grouped by type: count/shallow/retained (*)
    ///   "unclosed-resources"      — Closeable subclasses by instance count (spots unclosed handles)
    ///  (*) = needs full analysis; always available after load_dump
    ///
    /// OQL EXAMPLES (for custom queries):
    ///   "SELECT @objectId AS idx, @retainedHeapSize AS ret FROM java.lang.String ORDER BY ret DESC LIMIT 10"
    ///   "SELECT classof(x) AS class, COUNT(*) AS n FROM INSTANCEOF java.lang.Object x GROUP BY classof(x) ORDER BY n DESC LIMIT 20"
    ///
    /// OQL LANGUAGE FEATURES:
    ///
    /// BASIC SELECT:
    ///   SELECT @objectId, @usedHeapSize, @displayName FROM java.lang.Thread
    ///   SELECT * FROM INSTANCEOF java.util.List                 -- match subclasses too
    ///   SELECT @objectId FROM java.lang.String s WHERE s.value[0] > 65  -- array subscript in WHERE
    ///
    /// CAST & TYPE CHECKS:
    ///   SELECT CAST(@objectId AS STRING) FROM java.lang.Thread  -- cast to STRING, INT, LONG, FLOAT, BOOL
    ///   SELECT @displayName FROM java.lang.Object x WHERE x IS NOT NULL
    ///
    /// STRING FUNCTIONS:
    ///   SELECT upper(s.value), lower(s.value), length(s.value) FROM java.lang.String s
    ///   SELECT trim(s.value), substring(s.value, 0, 10) FROM java.lang.String s
    ///   SELECT concat(s.value, '!') FROM java.lang.String s WHERE s.value LIKE '%error%'
    ///
    /// AGGREGATES & GROUP BY:
    ///   SELECT classof(x), COUNT(*), SUM(@usedHeapSize) FROM INSTANCEOF java.lang.Object x GROUP BY classof(x)
    ///   SELECT @displayName, COUNT(*) AS n FROM INSTANCEOF java.lang.Object GROUP BY @displayName ORDER BY n DESC
    ///   SELECT classof(x), COUNT(DISTINCT @usedHeapSize) FROM INSTANCEOF java.lang.Object x GROUP BY classof(x)
    ///   SELECT MIN(@usedHeapSize), MAX(@usedHeapSize), AVG(@usedHeapSize) FROM java.lang.String
    ///   SELECT PERCENTILE(@usedHeapSize, 50) AS p50 FROM java.lang.String
    ///
    /// VALUES INLINE TABLE (no dump scan needed):
    ///   SELECT * FROM VALUES (1, 'a'), (2, 'b') AS t(id, name)
    ///   SELECT id * 2 FROM VALUES (10), (20), (30) AS t(id) ORDER BY id DESC
    ///
    /// JOIN (combining a heap scan with inline data):
    ///   SELECT t.name, v.label
    ///     FROM java.lang.Thread t
    ///     INNER JOIN VALUES ('main', 'primary'), ('worker', 'pool') AS v(name, label)
    ///     ON t.name = v.name
    ///   SELECT t.name, v.label
    ///     FROM java.lang.Thread t
    ///     LEFT JOIN VALUES ('main', 'primary') AS v(name, label)
    ///     ON t.name = v.name     -- unmatched rows get NULL for v.label
    ///
    /// WINDOW FUNCTIONS (ORDER BY + LIMIT apply globally):
    ///   SELECT @objectId, ROW_NUMBER() OVER () AS rn FROM java.lang.String LIMIT 5
    ///   SELECT @objectId, RANK() OVER (ORDER BY @usedHeapSize DESC) AS rank FROM java.lang.String
    ///
    /// ARITHMETIC & MODULO:
    ///   SELECT @usedHeapSize % 1024 AS remainder FROM java.lang.String
    ///   SELECT @usedHeapSize / 1024.0 AS kb FROM java.lang.String
    ///
    /// UNION / INTERSECT / EXCEPT:
    ///   SELECT @objectId FROM java.lang.Thread UNION SELECT @objectId FROM java.lang.Runnable
    ///   SELECT @objectId FROM java.lang.Thread INTERSECT SELECT @objectId FROM java.lang.Runnable
    ///
    /// SUBQUERIES:
    ///   SELECT @objectId FROM java.lang.Thread WHERE @objectId IN (SELECT @objectId FROM java.lang.Thread WHERE name = 'main')
    ///   SELECT @objectId FROM (SELECT @objectId, name FROM java.lang.Thread) t WHERE t.name = 'main'
    ///
    /// WITH (CTEs):
    ///   WITH big AS (SELECT @objectId FROM java.lang.String WHERE @usedHeapSize > 100)
    ///   SELECT @objectId FROM big
    pub oql: String,
    /// Include GC-unreachable objects in results. By default, only GC-reachable
    /// objects are returned (Eclipse MAT parity). Set to true to include all
    /// objects regardless of reachability — useful when queries return fewer
    /// rows than expected.
    pub all: Option<bool>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct BrowseDominatorsParams {
    /// Dense object index to start from. Use @objectId values from query results.
    /// Omit (or pass null) to start at the GC root — the recommended entry point.
    pub object_index: Option<ObjectIndex>,
    /// How many levels deep to expand (default 3, max 8).
    /// Use depth=1 to expand only immediate children of a node.
    pub depth: Option<u8>,
    /// Max children per node, sorted by retained size descending (default 10, max 50).
    pub width: Option<u8>,
    /// Optional session-epoch guard (H1). Every load_dump bumps a monotonically
    /// increasing `session_epoch` that is stamped into every tool response. Pass
    /// the epoch you saw when you collected `object_index`; if the dump has been
    /// reloaded since, the call fails with a `stale session` error instead of
    /// silently resolving the index against a different dump.
    #[serde(default)]
    pub expected_epoch: Option<u64>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct InspectObjectParams {
    /// Dense object index. Obtain from: (a) @objectId column in a query result,
    /// (b) the "index" field in browse_dominators output.
    pub object_index: ObjectIndex,
    /// Optional session-epoch guard (see browse_dominators for details).
    #[serde(default)]
    pub expected_epoch: Option<u64>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct FindIncomingReferencesParams {
    /// Dense object index to look up incoming references for. Obtain from:
    /// (a) @objectId column in a query result, (b) 'index' in browse_dominators.
    pub object_index: ObjectIndex,
    /// Max references to return (default 50, max 200).
    pub limit: Option<usize>,
    /// Optional session-epoch guard (see browse_dominators for details).
    #[serde(default)]
    pub expected_epoch: Option<u64>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct FindIncomingRefsByClassParams {
    /// Class name to aggregate incoming references for.
    /// Substring match: "String" matches "java.lang.String", "HashMap" matches both
    /// "java.util.HashMap" and "java.util.LinkedHashMap".
    pub class_name: String,
    /// Max source-class rows to return (default 20, max 100).
    pub limit: Option<usize>,
}

/// Parameters for `get_dominator_path` — walk idom[] up to the GC root.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct GetDominatorPathParams {
    /// Dense object index to start from. Obtain from: (a) @objectId column in a
    /// query result, (b) 'index' field in browse_dominators output.
    pub object_index: ObjectIndex,
    /// Optional session-epoch guard (see browse_dominators for details).
    #[serde(default)]
    pub expected_epoch: Option<u64>,
}

/// Parameters for `get_outbound_references` — forward CSR edges for one object.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct GetOutboundReferencesParams {
    /// Dense object index to look up outbound references for.
    pub object_index: ObjectIndex,
    /// Max references to return (default 50, max 200).
    pub limit: Option<usize>,
    /// Optional session-epoch guard (see browse_dominators for details).
    #[serde(default)]
    pub expected_epoch: Option<u64>,
}

/// Parameters for `get_field_name` — resolve a reference edge to its Java field name.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct GetFieldNameParams {
    /// Dense object index of the source object.
    pub src_object_index: SrcObjectIndex,
    /// 0-based index of the outbound edge within this object's edge list
    /// (i.e. position within `fwd_off[src]..fwd_off[src+1]`).
    pub edge_index: EdgeIndex,
    /// Optional session-epoch guard (see browse_dominators for details).
    #[serde(default)]
    pub expected_epoch: Option<u64>,
}

/// Parameters for `get_retained_set` — DFS dominator subtree enumeration.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct GetRetainedSetParams {
    /// Dense object index whose retained set to enumerate.
    pub object_index: ObjectIndex,
    /// Optional class name filter (substring match). Only objects whose class
    /// name contains this string are included in the results.
    pub class_filter: Option<String>,
    /// Max objects to return (default 50, max 500). The traversal visits
    /// children sorted by retained size descending, so the most significant
    /// objects are returned first.
    pub limit: Option<usize>,
    /// Optional session-epoch guard (see browse_dominators for details).
    #[serde(default)]
    pub expected_epoch: Option<u64>,
}

/// Parameters for `compare_dumps` — cross-dump regression detection.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct CompareDumpsParams {
    /// Absolute path to the baseline (older) .hprof file.
    pub baseline: String,
    /// Absolute path to the current (newer) .hprof file.
    pub current: String,
    /// Max growth-leader classes to return (default 20).
    pub limit: Option<usize>,
}

/// Parameters for `get_gc_roots`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct GetGcRootsParams {
    /// Max roots to return (default 500).
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct RedactParams {
    /// Absolute path to the input .hprof file (also accepts .hprof.gz, .hprof.zip, .tgz).
    pub input: String,
    /// Absolute path for the redacted output file.
    /// Extension controls compression: .hprof (raw), .hprof.gz (gzip), .hprof.zip (zip).
    /// Recommended: same name with "-redacted" suffix, e.g. "/tmp/dump-redacted.hprof".
    pub output: String,
    /// Use complete (two-pass) redaction: also zeros int/long/float/double/boolean/short/char
    /// scalar fields on object instances and static fields on classes.
    /// Default (false) is lean mode: single pass, zeros only primitive array elements
    /// (byte[], int[], long[], char[], etc.) — faster and uses less memory.
    /// Use complete=true when you need the strongest guarantee that no scalar values remain.
    #[serde(default)]
    pub complete: bool,
}

// ── Dominator tree node ───────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
struct DomNode {
    index: u64,
    class: String,
    retained_bytes: u64,
    shallow_bytes: u32,
    /// Total immediate children before width-truncation (0 when depth budget
    /// prevented descending, i.e. we never read the child CSR at this node).
    #[serde(skip_serializing_if = "is_zero_usize")]
    total_children: usize,
    /// True when `children` was truncated to `width` siblings.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    children_truncated: bool,
    children: Vec<DomNode>,
}

fn is_zero_usize(n: &usize) -> bool {
    *n == 0
}

// ── MCP server ────────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct HprofMcpServer {
    session: Arc<Mutex<Option<CachedSession>>>,
    /// Session epoch (H1): bumped on every SUCCESSFUL load_dump (including a
    /// same-path reload). Stamped into every tool response as `session_epoch`
    /// so an agent can detect that indices it collected earlier now refer to a
    /// different dump; index-taking tools accept an optional `expected_epoch`
    /// and fail with a `stale session` error on mismatch.
    ///
    /// Plain atomic (not behind the FIFO gate): all stateful handlers acquire
    /// the gate before reading it, so they observe it consistently with the
    /// session; load_dump bumps it while still holding the gate. Atomic also
    /// lets the two stateless tools (get_oql_docs, list_views) stamp it.
    epoch: Arc<std::sync::atomic::AtomicU64>,
    /// FIFO gate serializing stateful tool handlers in request-arrival order.
    ///
    /// rmcp spawns one tokio task per incoming request, so handlers run
    /// concurrently with no ordering guarantee. The session mutex protects
    /// consistency but is only held at the very end of `load_dump`, so a
    /// `get_session_info` arriving mid-load would acquire it, see `None`, and
    /// wrongly report `loaded:false` while a load is seconds from finishing.
    /// This gate is held for the *entire* duration of each stateful handler
    /// (including `load_dump`'s slow `spawn_blocking` region), so a later read
    /// queues behind an in-flight write and answers after it resolves.
    ///
    /// `tokio::sync::Mutex` is required (not `std`): it is fair (ordered
    /// wakeups — that IS the fix) and its guard is `Send` across `.await`,
    /// which a `std::sync::MutexGuard` is not.
    gate: Arc<Mutex<()>>,
    #[allow(dead_code)]
    tool_router: ToolRouter<HprofMcpServer>,
}

impl HprofMcpServer {
    pub fn new() -> Self {
        Self {
            session: Arc::new(Mutex::new(None)),
            epoch: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            gate: Arc::new(Mutex::new(())),
            tool_router: Self::tool_router(),
        }
    }

    /// Current session epoch (0 = no dump loaded in this session yet).
    fn epoch(&self) -> u64 {
        self.epoch.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// H1: when the caller passed `expected_epoch`, verify the session has not
    /// been switched to a different dump since. Must be called while the FIFO
    /// gate is held (same epoch-visibility contract as the handlers themselves).
    fn check_expected_epoch(&self, expected: Option<u64>) -> Result<(), McpError> {
        if let Some(x) = expected {
            let e = self.epoch();
            if e != x {
                return Err(McpError::invalid_params(
                    format!(
                        "stale session: dump changed (epoch {e} ≠ expected {x}); \
                         re-query indices from the current dump"
                    ),
                    None,
                ));
            }
        }
        Ok(())
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
                       After loading, call get_report({section:\"triage\"}) for severity-tagged diagnosis, then get_report({section:\"leaks\"}) to investigate. \
                       After loading, jsonify() and array subscript in OQL queries will return actual object data \
                       (requires native file system access; not available in the web version). \
                       Set collections=true (or full_analysis=true) up front if you expect to need collection-waste \
                       analysis (over-capacity collections, fill ratios) — the on-disk cache is keyed by dump path \
                       ONLY, not by these options, so a dump already cached under different options silently keeps \
                       serving the OLD options' data; ask the user to run `hprof-analyzer heap cache-clear <dump>` \
                       if a section you expect to be populated comes back empty. \
                       Set with_graph=true to enable find_incoming_references (full inbound-reference traversal \
                       covering every object, no significance floor) and the inbound-refs list on inspect_object. \
                       D1: a plain .hprof that ended mid-record returns an error by default; set force=true to \
                       load the partial data anyway (the ⚠ TRUNCATED warning is still included). \
                       Gzip/zip/tgz inputs are always loaded leniently regardless of force."
    )]
    async fn load_dump(
        &self,
        Parameters(p): Parameters<LoadDumpParams>,
    ) -> Result<CallToolResult, McpError> {
        // Held for the whole load (including the spawn_blocking region below) so a
        // later get_session_info queues behind it and sees the loaded dump.
        let _gate = self.gate.lock().await;
        let path = PathBuf::from(&p.path);
        // I-01: validate extension to prevent path traversal via symlinks to
        // arbitrary files (e.g. /etc/passwd).  Only .hprof, .hprof.gz, and
        // .hprof.zip are valid heap dump paths.
        let ext_ok = p.path.ends_with(".hprof")
            || p.path.ends_with(".hprof.gz")
            || p.path.ends_with(".hprof.zip")
            || p.path.ends_with(".tgz");
        if !ext_ok {
            return Err(McpError::invalid_params(
                "path must end with .hprof, .hprof.gz, .hprof.zip, or .tgz",
                None,
            ));
        }
        // D2: a relative path resolves against the SERVER's working directory,
        // which differs from the calling agent's cwd — a bare ENOENT gives no
        // clue about that. Reject relative paths with the hint up front, and
        // append the same hint when an absolute path is simply not found.
        if !Path::new(&p.path).is_absolute() {
            return Err(McpError::invalid_params(
                format!(
                    "path '{}' is relative — the MCP server resolves paths against its own \
                     working directory, which differs from yours; pass an absolute path \
                     (e.g. /full/path/to/dump.hprof)",
                    p.path
                ),
                None,
            ));
        }
        let mode = if p.with_graph {
            CacheMode::Graph
        } else {
            CacheMode::Full
        };
        let categories =
            parse_category_specs(&p.categories).map_err(|e| McpError::invalid_params(e, None))?;
        let collections = p.collections || p.full_analysis;
        let find_duplicates = p.find_duplicates || p.full_analysis;
        let opts = AnalyzeOptions {
            collections,
            find_duplicates,
            categories,
            // with_graph=true must build the field-name pool so get_field_name /
            // heap field-name work. obj_graph is the switch that gates
            // `want_field_names` in pass2 (capped at FIELD_NAME_EDGE_CAP there, so
            // huge heaps stay safe); selecting CacheMode::Graph alone never set it.
            obj_graph: p.with_graph,
            ..AnalyzeOptions::default()
        };
        let session_ref = Arc::clone(&self.session);
        let result = tokio::task::spawn_blocking(move || {
            crate::analyze_with_cache(&path, &opts, mode, |_| {})
        })
        .await
        .map_err(|e| McpError::internal_error(e.to_string(), None))?
        .map_err(|e| {
            // D2: an absolute path that doesn't exist on the server host gets
            // the same "whose filesystem?" hint, since agents commonly assume
            // path parity with their own machine.
            let msg = e.to_string();
            let is_not_found = e.kind() == std::io::ErrorKind::NotFound
                || msg.contains("No such file or directory");
            if is_not_found {
                McpError::invalid_params(
                    format!(
                        "{msg}\nhint: the MCP server reads paths on ITS host — verify the \
                         dump exists there (absolute path required, e.g. \
                         /full/path/to/dump.hprof)"
                    ),
                    None,
                )
            } else {
                McpError::internal_error(msg, None)
            }
        })?;

        // D1: refuse a truncated plain .hprof dump unless force=true.
        // Gzip/zip/tgz stays lenient (archives may hide the true EOF); this
        // gate mirrors the CLI's --force behaviour so MCP callers are equally
        // protected against silent partial-analysis results.
        if result.report.truncated_input && !p.force {
            let is_plain = {
                use std::io::Read;
                std::fs::File::open(&p.path)
                    .ok()
                    .and_then(|mut f| {
                        let mut h = [0u8; 4];
                        f.read_exact(&mut h).ok().map(|()| h)
                    })
                    .map(|h| h[..2] != [0x1f, 0x8b] && h[..2] != [0x50, 0x4b])
                    .unwrap_or(true)
            };
            if is_plain {
                return Err(McpError::invalid_params(
                    format!(
                        "dump is truncated — '{}' ended mid-record after {} bytes; \
                         the analysis covers only the objects read before truncation and \
                         is INCOMPLETE. Pass force=true to load the partial data anyway.",
                        p.path, result.pass1.file_size,
                    ),
                    None,
                ));
            }
        }

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

        let truncated_note: String = if result.report.truncated_input {
            format!(
                "\n⚠ TRUNCATED DUMP — file ended mid-record after {} bytes; \
                 this analysis covers only {} objects read before truncation and is INCOMPLETE. \
                 Re-copy the dump for a full analysis.\n",
                result.pass1.file_size, result.report.overview.total_objects,
            )
        } else {
            String::new()
        };

        // I-02: warn when requested options don't match what the cached report contains.
        let mut cache_mismatch_notes: Vec<&str> = Vec::new();
        if collections
            && result
                .report
                .collections
                .collection_fill_ratio
                .buckets
                .is_empty()
        {
            cache_mismatch_notes
                .push("collections=true requested but cache has no collection data");
        }
        if p.with_graph && result.mode != CacheMode::Graph {
            cache_mismatch_notes
                .push("with_graph=true requested but cache was built without the reference graph");
        }
        let cache_mismatch_note: String = if cache_mismatch_notes.is_empty() {
            String::new()
        } else {
            format!(
                "\n⚠ CACHE OPTIONS MISMATCH — {} — run `hprof-analyzer heap cache-clear {}` then reload.\n",
                cache_mismatch_notes.join("; "),
                p.path,
            )
        };

        let oql_hints: String = if result.report.leaks.suspects.is_empty() {
            String::new()
        } else {
            let mut s = String::from("\n## OQL — drill into top suspect\n");
            let top = &result.report.leaks.suspects[0];
            s.push_str(&format!(
                "query({{\"oql\":\"SELECT @objectId AS idx, @retainedHeapSize AS ret \
                 FROM {} ORDER BY ret DESC LIMIT 10\"}})\n\
                 → use idx with browse_dominators or inspect_object\n",
                top.pretty_class
            ));
            s
        };

        let summary = format!(
            "Loaded: {path}\n\
             Total heap: {heap} MB  |  Objects: {objs}\
             {truncated_note}\
             {redacted_note}\
             {cache_mismatch_note}\n\
             ## Top Leak Suspects ({n_suspects} detected)\n\
             {suspects}\n\
             ## Top Classes by Retained Size (histogram — not suspects)\n\
             These are the largest classes by retained heap; they may or may not be the leak root.\n\
             {classes}\
             {oql_hints}\n\
             If the above answers the question, stop here.\n\
             For leak/OOM investigation: get_report({{\"section\":\"triage\"}}) → then follow each critical/warning signal.",
            path = p.path,
            heap = result.report.overview.total_shallow / 1_000_000,
            objs = result.report.overview.total_objects,
            truncated_note = truncated_note,
            redacted_note = redacted_note,
            cache_mismatch_note = cache_mismatch_note,
            n_suspects = result.report.leaks.suspects.len(),
            suspects = suspects_text,
            classes = top_classes_text,
            oql_hints = oql_hints,
        );
        *session_ref.lock().await = Some(result);
        let new_epoch = self.epoch.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        let summary = format!("{summary}\nsession_epoch: {new_epoch}");
        Ok(CallToolResult::success(vec![ContentBlock::text(summary)]))
    }

    /// Return a section of the full analysis report as JSON.
    #[tool(description = "Return a report section. \
                          \n\n⚠ INDEX NOTE: obj_index_1based in leaks JSON (dominator_tree + root_path) is 1-BASED — subtract 1 before passing to browse_dominators/inspect_object. browse_dominators 'index' and query @objectId are 0-based (use directly).\
                          \n\nFOR SIMPLE QUESTIONS (just 1-2 calls after load_dump):\
                          \n  \"top-classes\"  — top N classes by retained size (use for 'what's using the most memory?')\
                          \n  \"top-objects\"  — top N biggest individual objects by retained size\
                          \n  \"overview\"     — heap totals, object count, identifier size\
                          \n  \"threads\"      — per-thread retained sizes + stack traces\
                          \n\nFOR LEAK INVESTIGATION (start here when asked to find a leak or OOM):\
                          \n  \"triage\"       — ⭐ severity-tagged signals (critical/warning/info); BEST first call for diagnosis\
                          \n                    After reading triage: act on EVERY critical/warning item by its id:\
                          \n                    headline-retainer/threadlocal-leak → get_report(leaks); off-heap → get_report(indicators);\
                          \n                    gc-waste → query(heap-summary); component-retention-imbalance → get_report(components)\
                          \n  \"leaks\"        — suspects with root_path, dominated objects, dominator_tree\
                          \n  \"retainers\"    — top stack frames/fields by retained size (who is keeping things alive)\
                          \n  \"dominators\"   — big-drop objects (retain >> largest child)\
                          \n\nOTHER SECTIONS:\
                          \n  \"waste\"        — reclaimable memory: duplicate strings, empty collections\
                          \n  \"indicators\"   — anon classes, ThreadLocal null keys, DirectByteBuffer total\
                          \n  \"arrays\"       — array length distribution\
                          \n  \"collections\"  — fill ratios, map load factors\
                          \n  \"references\"   — Soft/Weak/Phantom reference counts\
                          \n  \"components\"   — retained heap per class loader\
                          \n  \"gc-roots\"     — GC root counts + retained bytes by root type\
                          \n  \"classloaders\" — classloader count + loader rollup + duplicate classes\
                          \n  \"strings\"      — String/char[] counts + duplicate waste stats\
                          \n  \"packages\"     — biggest_packages tree (which package dominates memory)\
                          \n  \"culprits\"     — top-N synthesized likely culprit objects\
                          \n  \"self-referential\" — classes with many self-referential instances (list/trie node leaks)\
                          \n  \"static-collections\" — collection classes (ArrayList/HashMap/…) with instances/shallow/retained/ratio; high ratio reveals accumulation\
                          \n  \"resources\"    — file/stream/JDBC handle classes with instance counts; high counts reveal unclosed-resource leaks\
                          \n  \"alloc_sites\", \"thread_locals\", \"framework\", \"field_stats\", \"categories\"\
                          \n  \"top\"          — LARGE (full biggest-objects + biggest-classes); prefer \"top-objects\"/\"top-classes\"\
                          \n  \"all\"          — everything merged (very large — avoid in LLM workflows)\
                          \n\nDEFAULT: omitting `section` returns \"triage\" (changed from \"all\" in 0.3.4 — use \"all\" explicitly for the full report).")]
    async fn get_report(
        &self,
        Parameters(p): Parameters<GetReportParams>,
    ) -> Result<CallToolResult, McpError> {
        let _gate = self.gate.lock().await;
        let guard = self.session.lock().await;
        let sess = guard.as_ref().ok_or_else(|| {
            McpError::invalid_params("No dump loaded. Call load_dump first.", None)
        })?;
        let section = p.section.as_deref().unwrap_or("triage");
        let result = crate::heap_ops::report_section_with_limit(sess, section, p.limit)
            .map_err(ops_to_mcp)?;
        let hint = format!(
            "Report section '{section}' exceeded the {MAX_TOOL_OUTPUT_BYTES}-byte output cap and was \
             replaced with this stub. Request a focused section (triage, top-objects, top-classes, \
             leaks) or pass a smaller limit instead."
        );
        let json = serialize_capped(&result, &hint)?;
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
        let _gate = self.gate.lock().await;
        let guard = self.session.lock().await;
        let sess = guard.as_ref().ok_or_else(|| {
            McpError::invalid_params("No dump loaded. Call load_dump first.", None)
        })?;
        let limit = p.limit.unwrap_or(50).min(500) as usize;
        let result = crate::heap_ops::histogram(sess, limit).map_err(ops_to_mcp)?;
        let json = serde_json::to_string_pretty(&result)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        Ok(CallToolResult::success(vec![ContentBlock::text(json)]))
    }

    /// Run an OQL query against the loaded dump. Returns {columns, rows, truncated, row_count}.
    /// Rows are arrays of plain JSON values (strings, numbers, null). Object references
    /// appear as {"class":"ClassName","index":N} — use the index with inspect_object or browse_dominators.
    #[tool(description = "Run an OQL query OR a built-in view by name. \
                       SHORTCUT: pass a view name like \"top-retained-by-class\" instead of writing OQL. \
                       All 28 view names are listed in the oql parameter description — use them directly. \
                       Returns {columns, rows, truncated, row_count, view_name?}. \
                       Rows are plain JSON. Object references appear as {\"class\":\"ClassName\",\"index\":N} — use 'index' as object_index. \
                       Always SELECT @objectId AS idx to get indices for browse_dominators/inspect_object. \
                       Tip: SELECT jsonify(x) FROM ... expands objects to JSON trees (requires load_dump to have been called first, \
                       which builds the needed offset index automatically). \
                       Array subscript s[0]/s[0:3] also requires the offset index from load_dump. \
                       If a query returns fewer rows than expected (e.g. 0 rows for a class that COUNT(*) finds), \
                       try all=true to include GC-unreachable objects (raw heap scan).")]
    async fn query(
        &self,
        Parameters(p): Parameters<QueryParams>,
    ) -> Result<CallToolResult, McpError> {
        let _gate = self.gate.lock().await;
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
        let include_all = p.all.unwrap_or(false);

        let result = tokio::task::spawn_blocking(move || {
            // Defense in depth: even with subquery routing fixed, a latent panic
            // in parse/plan/run must not unwind through the stdio dispatch loop
            // and take down the whole server. Convert it to a per-request JSON
            // error row instead. (Effective only under `panic = "unwind"`; the
            // release profile uses `panic = "abort"`, where the real safeguard is
            // correct subquery routing in run_query_on_dump, not this guard.)
            let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                run_query_on_dump(&dump_path, &oql, include_all)
            }));
            match caught {
                Ok(r) => r,
                Err(panic) => {
                    let msg = panic
                        .downcast_ref::<&str>()
                        .map(|s| s.to_string())
                        .or_else(|| panic.downcast_ref::<String>().cloned())
                        .unwrap_or_else(|| "query evaluation panicked".to_string());
                    Ok(serde_json::json!({
                        "columns": [],
                        "rows": [],
                        "truncated": false,
                        "error": format!("internal error evaluating OQL (recovered): {msg}"),
                    }))
                }
            }
        })
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
        let output = serialize_capped(
            &result,
            "Query result exceeded the 256000-byte output cap and was replaced with this stub. \
             Narrow the query (add a WHERE filter or smaller LIMIT, select fewer columns) \
             instead of fetching every row.",
        )?;
        Ok(CallToolResult::success(vec![ContentBlock::text(output)]))
    }

    /// Browse the dominator tree. Omit object_index to start at the GC root.
    #[tool(
        description = "Navigate the dominator tree. Each node shows what an object retains. \
                       Omit object_index to start at the GC root (recommended first call). \
                       The 'index' field in results is 0-based — use directly with browse_dominators or inspect_object. \
                       NOTE: obj_index_1based from get_report (dominator_tree/root_path) is 1-BASED — subtract 1 before using here. \
                        Children sorted by retained_bytes desc — follow the largest child to find the root cause. \
                        retained_bytes = everything kept alive exclusively by this object/subtree. \
                        NOTE: width is clamped to 50 (depth to 8); when siblings were dropped, the node \
                        reports children_truncated:true and total_children (count before truncation). \
                        To see more of a wide node, re-call browse_dominators on that node's index."
    )]
    async fn browse_dominators(
        &self,
        Parameters(p): Parameters<BrowseDominatorsParams>,
    ) -> Result<CallToolResult, McpError> {
        let _gate = self.gate.lock().await;
        let guard = self.session.lock().await;
        let sess = guard.as_ref().ok_or_else(|| {
            McpError::invalid_params("No dump loaded. Call load_dump first.", None)
        })?;
        self.check_expected_epoch(p.expected_epoch)?;
        let depth = p.depth.unwrap_or(3).min(8);
        let width = p.width.unwrap_or(10).min(50) as usize;
        let result = crate::heap_ops::browse(sess, p.object_index.map(|v| v.get()), depth, width)
            .map_err(ops_to_mcp)?;
        let json = serde_json::to_string_pretty(&result)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        Ok(CallToolResult::success(vec![ContentBlock::text(json)]))
    }

    /// Inspect a single object by dense index.
    #[tool(description = "Get class name and memory sizes for one object. \
                       object_index comes from: (a) @objectId column in a query result (0-based, use directly), \
                       (b) 'index' field in browse_dominators output (0-based, use directly), \
                       (c) obj_index_1based from get_report dominator_tree/root_path (1-BASED — subtract 1 first!). \
                       Returns shallow_bytes (object itself) and retained_bytes (everything kept alive only by this object). \
                       When the dump was loaded with load_dump (which builds the offset index automatically), \
                       also returns 'fields': [{class, name, value}] for instance objects. \
                       When the dump was loaded with load_dump({with_graph: true}), also returns \
                       'inbound_refs': {total, shown, truncated, refs:[{object_index, class, shallow_bytes, retained_bytes}]} \
                       — up to 20 objects that directly reference this one. \
                       For the full inbound list (all referrers, no cap), call find_incoming_references. \
                       After inspect, call browse_dominators({object_index: <same>}) to see what it retains.")]
    async fn inspect_object(
        &self,
        Parameters(p): Parameters<InspectObjectParams>,
    ) -> Result<CallToolResult, McpError> {
        let _gate = self.gate.lock().await;
        let guard = self.session.lock().await;
        let sess = guard.as_ref().ok_or_else(|| {
            McpError::invalid_params("No dump loaded. Call load_dump first.", None)
        })?;
        self.check_expected_epoch(p.expected_epoch)?;
        let result = crate::heap_ops::inspect(sess, p.object_index.get()).map_err(ops_to_mcp)?;
        let json = serde_json::to_string_pretty(&result)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        Ok(CallToolResult::success(vec![ContentBlock::text(json)]))
    }

    /// Show all objects that hold a direct reference TO the specified object.
    #[tool(
        description = "Find all objects that hold a direct reference to the specified object \
                          (MAT's \"List Objects with incoming References\"). \
                          Requires that the dump was loaded with load_dump({with_graph: true}); \
                          returns an error with instructions otherwise. \
                          Each result row has: object_index (dense index of the referrer), \
                          class, shallow_bytes, retained_bytes. \
                          Use object_index with browse_dominators to follow the retainer chain upward. \
                          Typical workflow: find a suspicious object with query/browse_dominators, \
                          then call find_incoming_references to see which field of which class holds it alive."
    )]
    async fn find_incoming_references(
        &self,
        Parameters(p): Parameters<FindIncomingReferencesParams>,
    ) -> Result<CallToolResult, McpError> {
        let _gate = self.gate.lock().await;
        let guard = self.session.lock().await;
        let sess = guard.as_ref().ok_or_else(|| {
            McpError::invalid_params("No dump loaded. Call load_dump first.", None)
        })?;
        self.check_expected_epoch(p.expected_epoch)?;
        let limit = p.limit.unwrap_or(50).min(200);
        let result = crate::heap_ops::find_incoming(sess, p.object_index.get(), limit)
            .map_err(ops_to_mcp)?;
        let json = serde_json::to_string_pretty(&result)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        Ok(CallToolResult::success(vec![ContentBlock::text(json)]))
    }

    /// Aggregate incoming references to all instances of a class, grouped by source class.
    #[tool(
        description = "For a given target class, find all objects that hold direct references \
                          to ANY instance of that class, grouped by source class with summed counts \
                          and retained sizes. \
                          This is the per-class equivalent of find_incoming_references — MAT's \
                          'List Objects with incoming References' aggregated across all instances. \
                          Requires load_dump({with_graph: true}). \
                          class_name is a substring match: 'String' matches 'java.lang.String'. \
                          Returns rows sorted by ref_count descending: {src_class, ref_count, \
                          total_src_retained, total_src_shallow}. \
                          Use this to answer 'who keeps all X instances alive?' in one call."
    )]
    async fn find_incoming_references_by_class(
        &self,
        Parameters(p): Parameters<FindIncomingRefsByClassParams>,
    ) -> Result<CallToolResult, McpError> {
        let _gate = self.gate.lock().await;
        let guard = self.session.lock().await;
        let sess = guard.as_ref().ok_or_else(|| {
            McpError::invalid_params("No dump loaded. Call load_dump first.", None)
        })?;

        let limit = p.limit.unwrap_or(20);
        let result = crate::heap_ops::find_incoming_by_class(sess, &p.class_name, limit)
            .map_err(ops_to_mcp)?;

        let json = serde_json::to_string_pretty(&result)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        Ok(CallToolResult::success(vec![ContentBlock::text(json)]))
    }

    /// Return information about the currently loaded dump (path, heap size, object count).
    /// Call this to check if a dump is already loaded before calling load_dump.
    #[tool(
        description = "Check if a heap dump is already loaded in this session. \
                       Returns {loaded:true, path, ...} if a dump is cached, or {loaded:false}. \
                       Skip this and call load_dump directly if you already know which file to load. \
                       Useful when you don't have a specific file path (e.g. 'analyze the dump I already loaded')."
    )]
    async fn get_session_info(&self) -> Result<CallToolResult, McpError> {
        // Queue behind any in-flight load_dump so a mid-load status poll answers
        // after the load resolves instead of wrongly reporting loaded:false.
        let _gate = self.gate.lock().await;
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
                    .unwrap_or(true);
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
                    "_next": "get_report({\"section\":\"triage\"}) — severity-tagged signals; fastest diagnosis"
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

    /// Redact a heap dump: zero primitive array contents and optionally scalar fields.
    #[tool(
        description = "Redact a Java heap dump (.hprof) so it is safe to share. \
                       DISCLAIMER: redaction zeros data values but does NOT remove class names, \
                       field names, method names, thread names, source file names, object graph \
                       structure, or JVM metadata. The result can still reveal the application's \
                       architecture, library versions, frameworks in use, and data-access patterns. \
                       It significantly reduces leaked data but provides no privacy guarantee — \
                       only share with parties who need to diagnose the memory issue.\n\
                       Two modes:\n\
                       - lean (default, complete=false): single pass, zeros all primitive array elements \
                         (byte[], char[], int[], long[], float[], double[], boolean[], short[]). \
                         Fast and memory-efficient. Object instance scalar fields (int, long, etc.) are left intact.\n\
                       - complete (complete=true): two-pass, zeros everything lean does PLUS all primitive \
                         scalar fields on every object instance and static/constant-pool fields on classes. \
                         Strongest guarantee; roughly 2× slower.\n\
                       Both modes preserve the complete object graph: class names, field names, object IDs, \
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
        // Serialize against other cache/dump operations so concurrent runs don't
        // thrash the same on-disk cache directory.
        let _gate = self.gate.lock().await;
        // I-01: validate input extension to prevent reading arbitrary files.
        let input_ext_ok = p.input.ends_with(".hprof")
            || p.input.ends_with(".hprof.gz")
            || p.input.ends_with(".hprof.zip")
            || p.input.ends_with(".tgz");
        if !input_ext_ok {
            return Err(McpError::invalid_params(
                "input path must end with .hprof, .hprof.gz, .hprof.zip, or .tgz",
                None,
            ));
        }
        // I-01: validate output extension to prevent writing to arbitrary files.
        let output_ext_ok = p.output.ends_with(".hprof")
            || p.output.ends_with(".hprof.gz")
            || p.output.ends_with(".hprof.zip");
        if !output_ext_ok {
            return Err(McpError::invalid_params(
                "output path must end with .hprof, .hprof.gz, or .hprof.zip",
                None,
            ));
        }
        let input = p.input.clone();
        let output = p.output.clone();
        let complete = p.complete;
        tokio::task::spawn_blocking(move || {
            use crate::source::HprofSource;
            use std::{fs::File, io};

            let source = HprofSource::from(input.as_str());
            let lower = output.to_ascii_lowercase();
            let progress = |_phase: &str, _fraction: f64| {};
            let mode = if complete {
                crate::redact::RedactMode::Complete
            } else {
                crate::redact::RedactMode::Lean
            };

            if lower.ends_with(".hprof.gz") {
                let file = File::create(&output)?;
                let gz = flate2::write::GzEncoder::new(file, flate2::Compression::best());
                crate::redact::redact(&source, gz, mode, progress)
            } else if lower.ends_with(".hprof.zip") {
                let file = File::create(&output)?;
                let mut zip = zip::ZipWriter::new(file);
                let opts = zip::write::SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Deflated);
                zip.start_file("dump.hprof", opts)
                    .map_err(io::Error::other)?;
                crate::redact::redact(&source, &mut zip, mode, progress)?;
                zip.finish().map_err(io::Error::other)?;
                Ok(())
            } else {
                let file = File::create(&output)?;
                crate::redact::redact(&source, file, mode, progress)
            }
        })
        .await
        .map_err(|e| McpError::internal_error(e.to_string(), None))?
        .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let mode_note = if p.complete {
            "Mode: complete (two-pass)\n\
             What was zeroed:\n\
             - All primitive array elements (byte[], int[], long[], char[], float[], double[], boolean[], short[])\n\
             - All primitive scalar fields on object instances (int, long, float, double, boolean, short, char, byte)\n\
             - Static primitive field values and constant pool primitive values"
        } else {
            "Mode: lean (single-pass, default)\n\
             What was zeroed:\n\
             - All primitive array elements (byte[], int[], long[], char[], float[], double[], boolean[], short[])\n\
             What was left intact:\n\
             - Primitive scalar fields on object instances (int, long, etc.)\n\
             Use complete=true to also zero instance scalar fields."
        };

        let msg = format!(
            "Redacted dump written to: {output}\n\n\
             {mode_note}\n\n\
             What was preserved (both modes):\n\
             - Class names, field names, method names\n\
             - Object graph structure (all reference links)\n\
             - Object IDs, class hierarchy, dominator tree\n\
             - Thread stacks and GC roots\n\n\
             The redacted file is safe to share. Load it with load_dump to verify the object graph \
             or run hprof-analyzer heap summary on it directly.\n\
             Note: duplicate-string and collection fill-ratio analyses will be skipped (data is zeroed).",
            output = p.output,
            mode_note = mode_note,
        );
        Ok(CallToolResult::success(vec![ContentBlock::text(msg)]))
    }

    /// Compare two heap dumps for regression detection.
    #[tool(
        description = "Compare two heap dumps (baseline vs current) for class-level retained-heap growth. \
                       Loads both dumps (from disk cache if available, otherwise runs full analysis — \
                       first load of each dump takes 5–15 min), then diffs their class histograms. \
                       Returns: growth_leaders (classes whose retained heap grew most), \
                       new_classes (classes absent in baseline, present in current), \
                       net_delta_retained (total retained change), and grown_suspects (leak suspects \
                       that grew or appeared). \
                       Use for regression-detecting workflows: 'did this deployment make the heap worse?' \
                       Equivalent to `hprof-analyzer compare reports r1.json r2.json` on the CLI."
    )]
    async fn compare_dumps(
        &self,
        Parameters(p): Parameters<CompareDumpsParams>,
    ) -> Result<CallToolResult, McpError> {
        // Serialize against other cache/dump operations (shares the on-disk cache).
        let _gate = self.gate.lock().await;
        let baseline_path = PathBuf::from(&p.baseline);
        let current_path = PathBuf::from(&p.current);
        let limit = p.limit.unwrap_or(20).min(100);

        // I-11: run both analyses concurrently instead of sequentially.
        let baseline_task = tokio::task::spawn_blocking({
            let path = baseline_path.clone();
            move || {
                crate::analyze_with_cache(
                    &path,
                    &AnalyzeOptions::default(),
                    CacheMode::Full,
                    |_| {},
                )
            }
        });
        let current_task = tokio::task::spawn_blocking({
            let path = current_path.clone();
            move || {
                crate::analyze_with_cache(
                    &path,
                    &AnalyzeOptions::default(),
                    CacheMode::Full,
                    |_| {},
                )
            }
        });
        let (baseline_result, current_result) = tokio::try_join!(baseline_task, current_task)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        let baseline_result =
            baseline_result.map_err(|e| McpError::internal_error(e.to_string(), None))?;
        let current_result =
            current_result.map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let base_r = &baseline_result.report;
        let curr_r = &current_result.report;

        // Build class → (baseline_retained, baseline_instances) maps.
        use std::collections::HashMap;
        let mut base_map: HashMap<&str, (u64, u64)> = HashMap::new();
        for h in &base_r.overview.histogram {
            base_map.insert(&h.pretty_class, (h.retained, h.instances));
        }
        let mut curr_map: HashMap<&str, (u64, u64)> = HashMap::new();
        for h in &curr_r.overview.histogram {
            curr_map.insert(&h.pretty_class, (h.retained, h.instances));
        }

        // Growth leaders: in both dumps, sorted by delta_retained descending.
        let mut growth_pairs: Vec<(&str, i64, u64, u64, u64, u64)> = curr_r
            .overview
            .histogram
            .iter()
            .filter_map(|h| {
                let (base_ret, base_inst) = base_map
                    .get(h.pretty_class.as_str())
                    .copied()
                    .unwrap_or((0, 0));
                let delta = h.retained as i64 - base_ret as i64;
                if delta > 0 {
                    Some((
                        h.pretty_class.as_str(),
                        delta,
                        base_ret,
                        h.retained,
                        base_inst,
                        h.instances,
                    ))
                } else {
                    None
                }
            })
            .collect();
        growth_pairs.sort_by_key(|a| std::cmp::Reverse(a.1));

        let growth_leaders: Vec<serde_json::Value> = growth_pairs
            .iter()
            .take(limit)
            .map(|(class, delta, base_ret, curr_ret, base_inst, curr_inst)| {
                serde_json::json!({
                    "class": class,
                    "baseline_retained": base_ret,
                    "current_retained": curr_ret,
                    "delta_retained": delta,
                    "baseline_instances": base_inst,
                    "current_instances": curr_inst,
                })
            })
            .collect();

        // New classes: present in current, absent from baseline.
        let mut new_classes: Vec<serde_json::Value> = curr_r
            .overview
            .histogram
            .iter()
            .filter(|h| !base_map.contains_key(h.pretty_class.as_str()))
            .map(|h| {
                serde_json::json!({
                    "class": h.pretty_class,
                    "current_retained": h.retained,
                    "current_instances": h.instances,
                })
            })
            .collect();
        new_classes.sort_by(|a, b| {
            b["current_retained"]
                .as_u64()
                .unwrap_or(0)
                .cmp(&a["current_retained"].as_u64().unwrap_or(0))
        });
        new_classes.truncate(limit);

        // Grown suspects: leak suspects in current that grew or are new.
        let mut grown_suspects: Vec<serde_json::Value> = curr_r
            .leaks
            .suspects
            .iter()
            .filter_map(|s| {
                let base_ret = base_map
                    .get(s.pretty_class.as_str())
                    .map(|(r, _)| *r)
                    .unwrap_or(0);
                let delta = s.retained as i64 - base_ret as i64;
                if delta > 0 || base_ret == 0 {
                    Some(serde_json::json!({
                        "class": s.pretty_class,
                        "baseline_retained": base_ret,
                        "current_retained": s.retained,
                        "delta_retained": delta,
                        "is_new": base_ret == 0,
                    }))
                } else {
                    None
                }
            })
            .collect();
        grown_suspects.sort_by(|a, b| {
            b["delta_retained"]
                .as_i64()
                .unwrap_or(0)
                .cmp(&a["delta_retained"].as_i64().unwrap_or(0))
        });
        grown_suspects.truncate(limit);

        // Compute totals.
        let base_total_retained: u64 = base_r.overview.histogram.iter().map(|h| h.retained).sum();
        let curr_total_retained: u64 = curr_r.overview.histogram.iter().map(|h| h.retained).sum();
        let net_delta = curr_total_retained as i64 - base_total_retained as i64;
        let gross_growth: i64 = growth_pairs.iter().map(|(_, d, ..)| d).sum();
        let base_total_objects: u64 = base_r.overview.histogram.iter().map(|h| h.instances).sum();
        let curr_total_objects: u64 = curr_r.overview.histogram.iter().map(|h| h.instances).sum();

        let result = serde_json::json!({
            "baseline": p.baseline,
            "current": p.current,
            "net_delta_retained": net_delta,
            "gross_growth_retained": gross_growth,
            "delta_total_objects": curr_total_objects as i64 - base_total_objects as i64,
            "growth_leaders": growth_leaders,
            "new_classes": new_classes,
            "grown_suspects": grown_suspects,
            "_hint": "growth_leaders shows classes with the largest increase in retained bytes. \
                      new_classes are classes added between dumps (possible new accumulation). \
                      grown_suspects are leak suspects that grew or appeared. \
                      To investigate a specific growing class: load_dump on the current dump, then \
                      query({oql: \"SELECT @objectId AS idx, @retainedHeapSize AS ret FROM <ClassName> \
                      ORDER BY ret DESC LIMIT 10\"}), then browse_dominators on the largest instances.",
        });

        let json = serde_json::to_string_pretty(&result)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        Ok(CallToolResult::success(vec![ContentBlock::text(json)]))
    }

    /// Walk the dominator tree upward from an object to the GC root.
    #[tool(
        description = "Walk the immediate-dominator chain from an object up to the GC root. \
                       Returns an array of objects from the starting object to the virtual root, \
                       each with class name and sizes. This is the retention chain: every object \
                       in the path dominates the starting object, meaning ALL references to the \
                       starting object pass through every node in this chain. \
                       Use this to answer 'why is this object retained?' — the chain shows each \
                       object that must be collected before this one can be freed. \
                       Typical workflow: find a suspect with query/browse_dominators, then call \
                       get_dominator_path to see the full chain to the GC root."
    )]
    async fn get_dominator_path(
        &self,
        Parameters(p): Parameters<GetDominatorPathParams>,
    ) -> Result<CallToolResult, McpError> {
        let _gate = self.gate.lock().await;
        let guard = self.session.lock().await;
        let sess = guard.as_ref().ok_or_else(|| {
            McpError::invalid_params("No dump loaded. Call load_dump first.", None)
        })?;
        self.check_expected_epoch(p.expected_epoch)?;

        let result =
            crate::heap_ops::dominator_path(sess, p.object_index.get()).map_err(ops_to_mcp)?;

        let json = serde_json::to_string_pretty(&result)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        Ok(CallToolResult::success(vec![ContentBlock::text(json)]))
    }

    /// List the objects that a given object directly references (outbound edges).
    #[tool(
        description = "List all objects that the specified object directly references \
                       (MAT's 'List Objects with outgoing References'). \
                       Requires that the dump was loaded with load_dump({with_graph: true}); \
                       returns an error with instructions otherwise. \
                       Each result row has: target_index (dense index of the referenced object), \
                       target_class, target_shallow, target_retained. \
                       Use this to understand what an object HOLDS — complements \
                       find_incoming_references (what holds IT). \
                       Typical workflow: inspect_object to see fields, then get_outbound_references \
                       to see where each reference field actually points."
    )]
    async fn get_outbound_references(
        &self,
        Parameters(p): Parameters<GetOutboundReferencesParams>,
    ) -> Result<CallToolResult, McpError> {
        let _gate = self.gate.lock().await;
        let guard = self.session.lock().await;
        let sess = guard.as_ref().ok_or_else(|| {
            McpError::invalid_params("No dump loaded. Call load_dump first.", None)
        })?;
        self.check_expected_epoch(p.expected_epoch)?;

        let limit = p.limit.unwrap_or(50);
        let result = crate::heap_ops::outbound_references(sess, p.object_index.get(), limit)
            .map_err(ops_to_mcp)?;

        let json = serde_json::to_string_pretty(&result)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        Ok(CallToolResult::success(vec![ContentBlock::text(json)]))
    }

    /// Look up the Java field name for a specific reference edge.
    #[tool(
        description = "Look up the Java field name for a specific outbound reference edge of an \
                       object. Requires that the dump was loaded with load_dump({with_graph: true}) \
                       and that --ref-paths was active during analysis (field-name capture is optional). \
                       Parameters: src_object_index — the source object; edge_index — the 0-based \
                       position within that object's outbound edge list (as returned by \
                       get_outbound_references). Returns the Java field name string, or an empty \
                       string if the edge was captured without a name (e.g. array elements, \
                       virtual edges to class loaders). Use get_outbound_references first to discover \
                       valid edge_index values for a given object."
    )]
    async fn get_field_name(
        &self,
        Parameters(p): Parameters<GetFieldNameParams>,
    ) -> Result<CallToolResult, McpError> {
        let _gate = self.gate.lock().await;
        let guard = self.session.lock().await;
        let sess = guard.as_ref().ok_or_else(|| {
            McpError::invalid_params("No dump loaded. Call load_dump first.", None)
        })?;
        let result =
            crate::heap_ops::field_name(sess, p.src_object_index.get(), p.edge_index.get())
                .map_err(ops_to_mcp)?;
        let json = serde_json::to_string_pretty(&result)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        Ok(CallToolResult::success(vec![ContentBlock::text(json)]))
    }

    /// Enumerate objects exclusively retained by a given object (its dominator subtree).
    #[tool(
        description = "Enumerate the retained set of an object: all objects that would be freed if \
                       this object were garbage collected (its dominator subtree). \
                       Returns objects sorted by retained size descending, with optional class \
                       name filtering. Each entry has: index, class, shallow_bytes, retained_bytes, \
                       depth (distance from the root object in the dominator tree). \
                       Also returns a class_summary: top classes by total retained bytes in the subtree. \
                       Use this to answer 'what exactly is this object keeping alive?' — for example, \
                       after finding a leak suspect, call get_retained_set to see the composition of \
                       its retained memory. \
                       Combine with class_filter to focus: get_retained_set({object_index: 42, class_filter: 'String'}) \
                       to see only String objects retained by object 42."
    )]
    async fn get_retained_set(
        &self,
        Parameters(p): Parameters<GetRetainedSetParams>,
    ) -> Result<CallToolResult, McpError> {
        let _gate = self.gate.lock().await;
        let guard = self.session.lock().await;
        let sess = guard.as_ref().ok_or_else(|| {
            McpError::invalid_params("No dump loaded. Call load_dump first.", None)
        })?;
        self.check_expected_epoch(p.expected_epoch)?;

        let limit = p.limit.unwrap_or(50);
        let result = crate::heap_ops::retained_set(
            sess,
            p.object_index.get(),
            limit,
            p.class_filter.as_deref(),
        )
        .map_err(ops_to_mcp)?;

        let json = serde_json::to_string_pretty(&result)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        Ok(CallToolResult::success(vec![ContentBlock::text(json)]))
    }

    /// Return the GC root objects: their dense indices and class names.
    #[tool(
        description = "Return the dense object indices and class names of all GC roots in the \
                       loaded heap dump, up to `limit` (default 500). GC roots are the entry \
                       points of the object retention graph — everything not reachable from a \
                       root is eligible for GC. Use the returned object_index values with \
                       browse_dominators or inspect_object to explore what each root retains. \
                       Does not require with_graph=true."
    )]
    async fn get_gc_roots(
        &self,
        Parameters(p): Parameters<GetGcRootsParams>,
    ) -> Result<CallToolResult, McpError> {
        let _gate = self.gate.lock().await;
        let guard = self.session.lock().await;
        let sess = guard.as_ref().ok_or_else(|| {
            McpError::invalid_params("No dump loaded. Call load_dump first.", None)
        })?;

        let limit = p.limit.unwrap_or(500);
        let result = crate::heap_ops::gc_roots(sess, limit).map_err(ops_to_mcp)?;

        let json = serde_json::to_string_pretty(&result)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        Ok(CallToolResult::success(vec![ContentBlock::text(json)]))
    }

    /// List the 28 built-in named query views, grouped by category.
    #[tool(
        description = "List all 28 built-in named OQL queries grouped by category (Overview, Strings, Collections, Threads, Retained, Exploration). \
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
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(
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
                AFTER READING TRIAGE: act on every critical/warning item by its id:\n\
                  headline-retainer/threadlocal-leak → get_report(leaks)\n\
                  off-heap → get_report(indicators) for DirectByteBuffer total\n\
                  gc-waste → query(heap-summary) to see what's accumulating\n\
                  component-retention-imbalance → get_report(components)\n\
             3. get_report({\"section\":\"leaks\"})           — root paths, dominated objects, dominator_tree\n\
             4. browse_dominators({object_index: ...})   — drill into accumulation point\n\
             5. get_report({\"section\":\"top-classes\"})     — which classes dominate memory\n\n\
             GRAPH TRAVERSAL PRIMITIVES (for iterative exploration):\n\
             - get_dominator_path({object_index: N})     — walk idom[] from object to GC root (retention chain)\n\
             - get_outbound_references({object_index: N})— forward edges: what does this object reference? (requires with_graph=true)\n\
             - get_retained_set({object_index: N})       — DFS dominator subtree: what would be freed if this object were collected?\n\
             Combine these with browse_dominators (downward tree walk) and find_incoming_references (backward edges) \n\
             for complete graph exploration.\n\n\
             ANSWERING SIMPLE QUESTIONS ('biggest objects', 'top classes', etc.):\n\
             - get_report({\"section\":\"top-objects\"})      — top 20 biggest objects by retained size\n\
             - get_report({\"section\":\"top-classes\"})      — top 20 classes by retained size\n\
             - get_report({\"section\":\"top-objects\", \"limit\":5}) — adjust count with limit\n\
             DO NOT use get_report({\"section\":\"top\"}) or get_report({\"section\":\"all\"}) — they return megabytes of data.\n\n\
             QUERY TIPS:\n\
             - Always SELECT @objectId to get indices for follow-up calls\n\
             - Object references in results appear as {\"class\":\"ClassName\",\"index\":N} — use 'index' as object_index\n\
             - @retainedHeapSize = everything kept alive by this object (most useful for leak detection)\n\
             - Use INSTANCEOF to match subclasses; GROUP BY classof(x) to aggregate"
                .to_string(),
        )
    }
}

// ── Error mapping ─────────────────────────────────────────────────────────────

/// Transport-level safety net: serialized tool output above this many bytes is
/// replaced with a small `{_truncated:true, _hint:...}` stub instead of
/// flooding the model context (the 3.8 MB "all" report and megabyte
/// "dominators"/query payloads).
const MAX_TOOL_OUTPUT_BYTES: usize = 256_000;

/// Serialize `val` to pretty JSON; if the result exceeds
/// [`MAX_TOOL_OUTPUT_BYTES`], return a compact `{"_truncated": true, "_hint": hint}`
/// stub instead. Well-behaved responses pass through byte-for-byte unchanged.
fn serialize_capped(val: &serde_json::Value, hint: &str) -> Result<String, McpError> {
    let json = serde_json::to_string_pretty(val)
        .map_err(|e| McpError::internal_error(e.to_string(), None))?;
    if json.len() <= MAX_TOOL_OUTPUT_BYTES {
        return Ok(json);
    }
    let stub = serde_json::json!({
        "_truncated": true,
        "_bytes": json.len(),
        "_limit_bytes": MAX_TOOL_OUTPUT_BYTES,
        "_hint": hint,
    });
    serde_json::to_string_pretty(&stub).map_err(|e| McpError::internal_error(e.to_string(), None))
}

fn ops_to_mcp(e: crate::heap_ops::HeapOpsError) -> McpError {
    use crate::heap_ops::HeapOpsError;
    match e {
        HeapOpsError::BadParam(s) => McpError::invalid_params(s, None),
        // H2: graph.bin cache misses mean the dump was not loaded with with_graph=true
        // (or the graph.bin was written by an older build). Surface an actionable message.
        HeapOpsError::CacheMissing(s) if s.contains("graph.bin") => McpError::invalid_params(
            format!(
                "reference graph not available ({s}) — reload the dump with \
                     load_dump({{\"path\":\"<path>\", \"with_graph\":true}}) to enable \
                     find_incoming_references and get_outbound_references; \
                     if that still fails, clear the cache first: \
                     `hprof-analyzer heap cache-clear <path>`"
            ),
            None,
        ),
        HeapOpsError::CacheMissing(s) | HeapOpsError::Internal(s) => {
            McpError::internal_error(s, None)
        }
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
        // The virtual GC root's retained is the total reachable heap — the sum of
        // every object's shallow size. Summing per-object retained sizes would
        // double-count dominated objects (each object's retained overlaps its
        // dominator's).
        let total: u64 = shallow.iter().map(|&s| s as u64).sum();
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
    let mut total_children = 0usize;
    let mut children_truncated = false;
    if depth > 0 {
        let i = idx as usize;
        if i + 1 < dc_off.len() {
            let start = dc_off[i] as usize;
            let end = dc_off[i + 1] as usize;
            total_children = end - start;
            let mut ch: Vec<u64> = (start..end).map(|j| dc_tgt[j] as u64).collect();
            ch.sort_by(|&a, &b| {
                let ra = retained.get(a as usize).copied().unwrap_or(0);
                let rb = retained.get(b as usize).copied().unwrap_or(0);
                rb.cmp(&ra)
            });
            children_truncated = ch.len() > width;
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
        total_children,
        children_truncated,
        children,
    }
}

// ── OQL query helper ──────────────────────────────────────────────────────────

fn run_query_on_dump(
    dump_path: &std::path::Path,
    oql: &str,
    all: bool,
) -> std::io::Result<serde_json::Value> {
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
    let uses_subqueries = flat_plans.iter().any(|(_, p)| {
        p.from_subplan.is_some() || !p.in_subplans.is_empty() || !p.exists_subplans.is_empty()
    });
    // VALUES-only queries have no heap FROM source; short-circuit them before the
    // heap scan so `SELECT * FROM (VALUES ...)` returns inline rows (mirrors
    // `run_oql_query` in lib.rs).
    if let Some(results) = query::run::run_values_only_shortcircuit(&flat_plans) {
        return Ok(serialize_query_results(results));
    }
    let (flat, union_groups) = query::run::expand_union_queries(&flat_plans);

    let dump_str = dump_path
        .to_str()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "non-UTF8 path"))?;
    let reachable_only = !all;
    let opts = AnalyzeOptions {
        reachable_only,
        ..AnalyzeOptions::default()
    };
    // Subquery-bearing plans must run through the two-phase pipeline to resolve
    // FROM/IN/EXISTS subplans before outer execution. The escalated single-pass
    // path expects those predicates pre-resolved and can panic if invoked here.
    let results = if uses_subqueries {
        query::run::run_single_dump(dump_str, &flat_plans, reachable_only)?
    } else {
        run_oql::run_oql_escalated(dump_str, &flat, &union_groups, reachable_only, &opts)?
    };

    let out = serialize_query_results(results);
    if out
        .get("row_count")
        .and_then(|v| v.as_u64())
        .is_some_and(|n| n == 0)
        && out.get("error").is_none()
    {
        if let Some(missing) = missing_exact_from_class(dump_path, &flat_plans[0].0)? {
            return Ok(serde_json::json!({
                "columns": [],
                "rows": [],
                "truncated": false,
                "row_count": 0,
                "error": format!("class `{missing}` not present in dump")
            }));
        }
    }

    Ok(out)
}

fn missing_exact_from_class(
    dump_path: &std::path::Path,
    q: &crate::query::ast::Query,
) -> std::io::Result<Option<String>> {
    use crate::query::ast::{FromSource, Predicate};

    fn collect_exact_from_classes(q: &crate::query::ast::Query, out: &mut Vec<String>) {
        if let FromSource::Class(spec) = &q.from {
            if !spec.is_regex && !spec.class_name.contains('*') {
                out.push(spec.class_name.clone());
            }
        }
        if let FromSource::Subquery(inner) = &q.from {
            collect_exact_from_classes(inner, out);
        }
        for b in &q.union_branches {
            collect_exact_from_classes(b, out);
        }
        for b in &q.intersect_branches {
            collect_exact_from_classes(b, out);
        }
        for b in &q.except_branches {
            collect_exact_from_classes(b, out);
        }
        if let Some(pred) = &q.where_ {
            collect_exact_from_classes_in_pred(pred, out);
        }
        if let Some(pred) = &q.having {
            collect_exact_from_classes_in_pred(pred, out);
        }
    }

    fn collect_exact_from_classes_in_pred(pred: &Predicate, out: &mut Vec<String>) {
        match pred {
            Predicate::And(a, b) | Predicate::Or(a, b) => {
                collect_exact_from_classes_in_pred(a, out);
                collect_exact_from_classes_in_pred(b, out);
            }
            Predicate::Not(a) => collect_exact_from_classes_in_pred(a, out),
            Predicate::InSubquery { inner, .. } | Predicate::Exists { inner, .. } => {
                collect_exact_from_classes(inner, out)
            }
            Predicate::Compare { .. } | Predicate::InstanceOf(_) | Predicate::Bool(_) => {}
        }
    }

    let mut wanted = Vec::new();
    collect_exact_from_classes(q, &mut wanted);
    wanted.sort();
    wanted.dedup();
    if wanted.is_empty() {
        return Ok(None);
    }

    let source =
        crate::source::HprofSource::from(dump_path.to_str().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "non-UTF8 path")
        })?);
    let p1 = crate::pass1::Pass1::run(&source, false)?;

    let mut known = std::collections::HashSet::with_capacity(p1.class_map.len());
    for ci in p1.class_map.values() {
        if let Some(raw) = p1.strings.get(&ci.name_id) {
            known.insert(raw.replace('/', "."));
        }
    }

    Ok(wanted
        .into_iter()
        .find(|name| !known.contains(&name.replace('/', "."))))
}

/// Serialize a single-query result list (first result) into the MCP `query`
/// tool's JSON shape: `{columns, rows, truncated, row_count, error?, note?}`.
fn serialize_query_results(results: Vec<crate::query::model::QueryResult>) -> serde_json::Value {
    if let Some(r) = results.into_iter().next() {
        let cols: Vec<&str> = r.columns.iter().map(|c| c.name.as_str()).collect();
        let rows: Vec<Vec<serde_json::Value>> = r
            .rows
            .iter()
            .map(|row| row.iter().map(query_value_to_json).collect())
            .collect();
        let mut out = serde_json::json!({
            "columns": cols,
            "rows": rows,
            "truncated": r.truncated,
            "row_count": r.row_count,
        });
        if let Some(e) = &r.error {
            out["error"] = serde_json::json!(e);
        }
        if let Some(n) = &r.note {
            out["note"] = serde_json::json!(n);
        }
        out
    } else {
        serde_json::json!({ "columns": [], "rows": [], "truncated": false })
    }
}

/// Convert a QueryValue to a plain JSON value suitable for LLM consumption.
/// ObjRef renders as {"class": "ClassName", "index": N} (a structured JSON object).
fn query_value_to_json(v: &crate::query::model::QueryValue) -> serde_json::Value {
    crate::heap_ops::query_value_to_json(v)
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
    // Looks like raw OQL — return the `/run `-stripped candidate so the caller
    // parses it as OQL (a leading "/run " would make the parser fail).
    (candidate, None)
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

/// Recursively cap `dominator_tree` / `children` depth and width in a JSON value.
/// Any `children` array found at depth >= max_depth is replaced with a truncation marker.
/// At any depth, children arrays are also capped to max_width entries.
pub(crate) fn truncate_dom_tree_depth(v: &mut serde_json::Value, max_depth: usize) {
    truncate_dom_tree_inner(v, 0, max_depth, 20);
}

fn truncate_dom_tree_inner(
    v: &mut serde_json::Value,
    depth: usize,
    max_depth: usize,
    max_width: usize,
) {
    match v {
        serde_json::Value::Object(map) => {
            if depth >= max_depth {
                // At or past the depth cap: wipe all children arrays.
                if let Some(children) = map.get_mut("children") {
                    let was_empty = children.as_array().map(|a| a.is_empty()).unwrap_or(true);
                    if !was_empty {
                        *children = serde_json::json!([{"_truncated": true}]);
                    }
                }
            } else {
                // Width cap: trim children array before recursing.
                if let Some(children) = map.get_mut("children") {
                    if let Some(arr) = children.as_array_mut() {
                        let total = arr.len();
                        if total > max_width {
                            arr.truncate(max_width);
                            arr.push(serde_json::json!({"_truncated": true, "_omitted": total - max_width}));
                        }
                    }
                }
                for (key, val) in map.iter_mut() {
                    // The `dominator_tree` key roots the actual tree; reset depth so
                    // the wrapper layers (leaks → suspects[]) don't consume the
                    // depth budget and silently cap the tree two levels early.
                    let child_depth = if key == "dominator_tree" {
                        0
                    } else {
                        depth + 1
                    };
                    truncate_dom_tree_inner(val, child_depth, max_depth, max_width);
                }
            }
        }
        serde_json::Value::Array(arr) => {
            for item in arr.iter_mut() {
                truncate_dom_tree_inner(item, depth, max_depth, max_width);
            }
        }
        _ => {}
    }
}

/// Remove low-signal fields from `dominated_by_class` entries in the leaks JSON.
/// Keeps: pretty_class, instances, retained. Drops: loader_id, loader_label,
/// max_instance_shallow, incoming_ref_count (rarely actionable for LLMs).
pub(crate) fn strip_dominated_by_class_noise(v: &mut serde_json::Value) {
    const KEEP: &[&str] = &["pretty_class", "instances", "retained"];
    if let Some(suspects) = v.get_mut("suspects").and_then(|s| s.as_array_mut()) {
        for suspect in suspects.iter_mut() {
            if let Some(dbc) = suspect
                .get_mut("dominated_by_class")
                .and_then(|d| d.as_array_mut())
            {
                for entry in dbc.iter_mut() {
                    if let Some(map) = entry.as_object_mut() {
                        map.retain(|k, _| KEEP.contains(&k.as_str()));
                    }
                }
            }
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

/// Delegate to the shared implementation in [`crate::heap_ops`].
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{cache::CacheMode, opts::AnalyzeOptions};

    /// Locate the philosophers fixture, or skip the test.
    fn philosophers_path() -> Option<std::path::PathBuf> {
        let p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/dump_4_philosophers.hprof");
        match std::fs::metadata(&p) {
            Ok(m) if m.len() >= 1024 => Some(p),
            _ => None,
        }
    }

    /// Load the philosophers dump from cache (fast if already analysed).
    fn load_session() -> Option<crate::cache::CachedSession> {
        let path = philosophers_path()?;
        crate::analyze_with_cache(&path, &AnalyzeOptions::default(), CacheMode::Full, |_| {}).ok()
    }

    #[test]
    fn parse_category_specs_valid_entries() {
        // More specific category ("legacy") must be listed FIRST to ever
        // match anything — same first-match-wins rule as classify.rs's own
        // more_specific_user_category_must_be_listed_first test.
        let specs = vec![
            "legacy=com.mycompany.legacy.".to_string(),
            "application=com.mycompany.".to_string(),
        ];
        let defs = parse_category_specs(&specs).expect("valid specs must parse");
        assert_eq!(defs.len(), 2);
        assert_eq!(defs[0].name, "legacy");
        assert_eq!(defs[1].name, "application");
        // Classify through the same path load_dump would use.
        let classifier = crate::classify::build_classifier(defs);
        assert_eq!(
            classifier.classify("com.mycompany.legacy.OldThing"),
            "legacy"
        );
        assert_eq!(classifier.classify("com.mycompany.NewThing"), "application");
        assert_eq!(classifier.classify("java.util.HashMap"), "jdk");
    }

    #[test]
    fn parse_category_specs_rejects_missing_equals() {
        let specs = vec!["application".to_string()];
        let err = parse_category_specs(&specs).expect_err("missing '=' must be rejected");
        assert!(err.contains("application"));
        assert!(err.contains("NAME=PREFIX"));
    }

    #[test]
    fn parse_category_specs_rejects_empty_name_or_prefix() {
        assert!(parse_category_specs(&["=com.mycompany.".to_string()]).is_err());
        assert!(parse_category_specs(&["application=".to_string()]).is_err());
    }

    #[test]
    fn parse_category_specs_empty_input_is_ok() {
        assert!(parse_category_specs(&[]).unwrap().is_empty());
    }

    #[test]
    fn strip_ansi_removes_escape_sequences() {
        assert_eq!(strip_ansi("\x1b[31mred\x1b[0m"), "red");
        assert_eq!(strip_ansi("plain"), "plain");
        assert_eq!(
            strip_ansi("\x1b[1;32mbold green\x1b[0m text"),
            "bold green text"
        );
        assert_eq!(strip_ansi(""), "");
    }

    #[test]
    fn threads_section_is_bounded() {
        let Some(sess) = load_session() else {
            return;
        };
        let r = &sess.report;
        let limit = 20usize;
        let max_frames = 20usize;

        let mut sorted: Vec<_> = r.threads.threads.iter().collect();
        sorted.sort_by_key(|t| std::cmp::Reverse(t.retained));
        let rows: Vec<serde_json::Value> = sorted
            .into_iter()
            .take(limit)
            .map(|t| {
                let frames: Vec<_> = t.frames.iter().take(max_frames).cloned().collect();
                serde_json::json!({
                    "thread_serial": t.thread_serial,
                    "name": t.name,
                    "retained": t.retained,
                    "frames": frames,
                })
            })
            .collect();

        let output = serde_json::json!({
            "total_threads": r.threads.threads.len(),
            "shown": rows.len(),
            "sorted_by": "retained_desc",
            "threads": rows,
        });

        assert!(output["total_threads"].as_u64().unwrap() > 0);
        assert!(output["shown"].as_u64().unwrap() <= limit as u64);
        assert_eq!(output["sorted_by"], "retained_desc");

        for thread in output["threads"].as_array().unwrap() {
            let frames = thread["frames"].as_array().unwrap();
            assert!(
                frames.len() <= max_frames,
                "frames exceeded cap: {}",
                frames.len()
            );
        }

        let retained_vals: Vec<u64> = output["threads"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["retained"].as_u64().unwrap_or(0))
            .collect();
        let mut sorted_check = retained_vals.clone();
        sorted_check.sort_unstable_by(|a, b| b.cmp(a));
        assert_eq!(
            retained_vals, sorted_check,
            "threads not sorted by retained desc"
        );
    }

    #[test]
    fn threads_section_limit_controls_count() {
        let Some(sess) = load_session() else {
            return;
        };
        let r = &sess.report;

        let limit = 5usize;
        let mut sorted: Vec<_> = r.threads.threads.iter().collect();
        sorted.sort_by_key(|t| std::cmp::Reverse(t.retained));
        let rows: Vec<_> = sorted.into_iter().take(limit).collect();

        assert!(rows.len() <= limit);
        if r.threads.threads.len() > limit {
            assert_eq!(rows.len(), limit);
        }
    }

    #[test]
    fn triage_section_returns_array() {
        let Some(sess) = load_session() else {
            return;
        };
        let r = &sess.report;

        let json = serde_json::to_string_pretty(&r.triage).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert!(
            parsed.is_array(),
            "triage section must serialize as an array"
        );
    }

    #[test]
    fn overview_section_has_heap_size_field() {
        let Some(sess) = load_session() else {
            return;
        };
        let r = &sess.report;

        let json = serde_json::to_string_pretty(&r.overview).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert!(
            parsed.get("total_heap_bytes").is_some()
                || parsed.get("heap_bytes").is_some()
                || parsed.get("total_shallow").is_some()
                || parsed.get("total_retained").is_some(),
            "overview missing heap size field; got keys: {:?}",
            parsed.as_object().map(|o| o.keys().collect::<Vec<_>>())
        );
    }

    #[test]
    fn build_query_hint_detects_objectid_column() {
        let result = serde_json::json!({
            "columns": ["class", "objectId", "retained"],
            "rows": [["java.lang.String", 42i64, 1024i64]],
        });

        let hint = build_query_hint(&result);
        assert!(hint.is_some(), "expected a hint for objectId column");
        let msg = hint.unwrap();
        assert!(msg.contains("42"), "hint should include example index");
        assert!(msg.contains("browse_dominators") || msg.contains("inspect_object"));
    }

    #[test]
    fn build_query_hint_no_hint_without_index_col() {
        let result = serde_json::json!({
            "columns": ["class", "retained"],
            "rows": [["java.lang.String", 1024i64]],
        });

        let hint = build_query_hint(&result);
        assert!(
            hint.is_none(),
            "no hint expected when no index column present"
        );
    }

    #[test]
    fn build_query_hint_empty_rows_returns_none() {
        let result = serde_json::json!({
            "columns": ["objectId"],
            "rows": [],
        });

        let hint = build_query_hint(&result);
        assert!(hint.is_none(), "no hint expected for empty rows");
    }

    #[test]
    fn resolve_view_or_oql_matches_known_view() {
        let (oql, name) = resolve_view_or_oql("largest-objects");
        assert!(name.is_some(), "should match known view");
        assert_eq!(name.unwrap(), "largest-objects");
        assert!(!oql.is_empty());
    }

    #[test]
    fn resolve_view_or_oql_case_insensitive() {
        let (_, name_lower) = resolve_view_or_oql("largest-objects");
        let (_, name_upper) = resolve_view_or_oql("Largest-Objects");
        assert_eq!(
            name_lower, name_upper,
            "case-insensitive match should return same view"
        );
    }

    #[test]
    fn resolve_view_or_oql_strips_run_prefix() {
        let (oql_raw, _) = resolve_view_or_oql("largest-objects");
        let (oql_run, name) = resolve_view_or_oql("/run largest-objects");
        assert!(name.is_some());
        assert_eq!(
            oql_raw, oql_run,
            "/run prefix should be stripped before matching"
        );
    }

    #[test]
    fn resolve_view_or_oql_passes_through_raw_oql() {
        let raw = "SELECT * FROM java.lang.String LIMIT 5";
        let (out, name) = resolve_view_or_oql(raw);
        assert!(name.is_none(), "should not match a view name");
        assert_eq!(out, raw);
    }

    #[test]
    fn resolve_view_or_oql_strips_run_prefix_before_raw_oql() {
        // "/run <sql>" must forward the raw SQL with the "/run " prefix
        // stripped, not treat the prefix as part of the query text (otherwise
        // the OQL parser can never succeed on it).
        let raw = "SELECT 1 FROM (VALUES (1)) AS t(n)";
        let input = format!("/run {raw}");
        let (out, name) = resolve_view_or_oql(&input);
        assert!(name.is_none(), "raw OQL after /run is not a named view");
        assert_eq!(
            out, raw,
            "the /run prefix must be stripped even when the remainder is raw OQL"
        );
    }

    #[test]
    fn query_values_only_query_returns_inline_rows() {
        // A VALUES-only query has no heap FROM source; it must return the
        // inline table directly without scanning (or even requiring) the heap.
        let dir = std::env::temp_dir();
        let mut path = dir;
        path.push(format!(
            "hprof_mcp_values_test_{}.hprof",
            std::process::id()
        ));
        let mut h = b"JAVA PROFILE 1.0.2\0".to_vec();
        h.extend_from_slice(&8u32.to_be_bytes()); // id_size
        h.extend_from_slice(&0u64.to_be_bytes()); // timestamp
        std::fs::write(&path, &h).unwrap();

        let result = run_query_on_dump(
            &path,
            "SELECT id FROM (VALUES (10),(20),(30)) AS t(id)",
            false,
        )
        .expect("VALUES-only query should not require a heap scan");
        let _ = std::fs::remove_file(&path);

        let rows = result
            .get("rows")
            .and_then(|r| r.as_array())
            .unwrap_or_else(|| panic!("expected rows array, got {result}"));
        assert_eq!(
            rows.len(),
            3,
            "VALUES-only query must return its 3 inline rows, not a heap scan; got {result}"
        );
    }

    #[test]
    fn query_values_string_literals_use_double_quotes() {
        // OQL string literals are double-quoted. This ensures VALUES examples
        // using strings work as documented.
        let dir = std::env::temp_dir();
        let mut path = dir;
        path.push(format!(
            "hprof_mcp_values_strings_test_{}.hprof",
            std::process::id()
        ));
        let mut h = b"JAVA PROFILE 1.0.2\0".to_vec();
        h.extend_from_slice(&8u32.to_be_bytes());
        h.extend_from_slice(&0u64.to_be_bytes());
        std::fs::write(&path, &h).unwrap();

        let result = run_query_on_dump(
            &path,
            "SELECT * FROM (VALUES (1, \"a\"), (2, \"b\")) AS t(n, s)",
            false,
        )
        .expect("VALUES string query should execute");
        let _ = std::fs::remove_file(&path);

        let rows = result
            .get("rows")
            .and_then(|r| r.as_array())
            .unwrap_or_else(|| panic!("expected rows array, got {result}"));
        assert_eq!(rows.len(), 2, "expected two inline VALUES rows: {result}");
    }

    #[test]
    fn query_join_values_works() {
        // JOIN against an inline VALUES table should parse and execute.
        let dir = std::env::temp_dir();
        let mut path = dir;
        path.push(format!(
            "hprof_mcp_join_values_test_{}.hprof",
            std::process::id()
        ));
        let mut h = b"JAVA PROFILE 1.0.2\0".to_vec();
        h.extend_from_slice(&8u32.to_be_bytes());
        h.extend_from_slice(&0u64.to_be_bytes());
        std::fs::write(&path, &h).unwrap();

        let result = run_query_on_dump(
            &path,
            "SELECT t.n, p.label FROM (VALUES (1), (5)) AS t(n) JOIN (VALUES (1, \"MIN\"), (5, \"NORM\")) AS p(n, label) ON t.n = p.n",
            false,
        )
        .expect("JOIN VALUES query should execute");
        let _ = std::fs::remove_file(&path);

        let rows = result
            .get("rows")
            .and_then(|r| r.as_array())
            .unwrap_or_else(|| panic!("expected rows array, got {result}"));
        assert_eq!(rows.len(), 2, "expected two joined rows: {result}");
        assert!(
            result.get("error").is_none(),
            "JOIN VALUES should not return an error: {result}"
        );
    }

    #[test]
    fn query_values_in_literal_list_works() {
        // Reproducer for IN-literal-list behavior on inline VALUES rows.
        let dir = std::env::temp_dir();
        let mut path = dir;
        path.push(format!(
            "hprof_mcp_values_in_list_test_{}.hprof",
            std::process::id()
        ));
        let mut h = b"JAVA PROFILE 1.0.2\0".to_vec();
        h.extend_from_slice(&8u32.to_be_bytes());
        h.extend_from_slice(&0u64.to_be_bytes());
        std::fs::write(&path, &h).unwrap();

        let result = run_query_on_dump(
            &path,
            "SELECT n FROM (VALUES (1), (2), (3)) AS t(n) WHERE n IN (1, 3)",
            false,
        )
        .expect("IN-list query should execute");
        let _ = std::fs::remove_file(&path);

        let rows = result
            .get("rows")
            .and_then(|r| r.as_array())
            .unwrap_or_else(|| panic!("expected rows array, got {result}"));
        assert_eq!(
            rows.len(),
            2,
            "expected two rows matching IN list: {result}"
        );
        assert!(
            result.get("error").is_none(),
            "IN-list query should not return an error: {result}"
        );
    }

    #[test]
    fn query_subquery_uses_two_phase_path_without_crash() {
        let result = run_query_on_dump(
            std::path::Path::new("tests/fixtures/dump_4_philosophers.hprof"),
            "SELECT s.@displayName FROM java.lang.String s WHERE s.@objectId IN (SELECT x.@objectId FROM java.lang.Thread x)",
            false,
        )
        .expect("subquery query should execute and return JSON");

        assert!(
            result.get("error").is_none(),
            "subquery query unexpectedly returned an error: {result}"
        );
        assert!(
            result.get("row_count").and_then(|v| v.as_u64()).is_some(),
            "expected row_count in result: {result}"
        );
    }

    #[test]
    fn query_unknown_exact_class_returns_explicit_error() {
        let result = run_query_on_dump(
            std::path::Path::new("tests/fixtures/dump_4_philosophers.hprof"),
            "SELECT * FROM com.does.not.Exist",
            false,
        )
        .expect("query should return JSON payload");

        let err = result
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or_else(|| panic!("expected explicit error for unknown class, got: {result}"));
        assert!(
            err.contains("not present in dump"),
            "unexpected error text: {err}"
        );
    }

    #[test]
    fn truncate_dom_tree_strips_deep_children() {
        let mut tree = serde_json::json!({
            "name": "root",
            "children": [
                {
                    "name": "depth1",
                    "children": [
                        {
                            "name": "depth2",
                            "children": [
                                {
                                    "name": "depth3",
                                    "children": [
                                        {"name": "depth4", "children": []}
                                    ]
                                }
                            ]
                        }
                    ]
                }
            ]
        });

        truncate_dom_tree_depth(&mut tree, 3);

        // depth 0 (root) and depth 1 should be intact
        let depth1 = &tree["children"][0];
        assert_eq!(depth1["name"], "depth1");

        // depth 2 children should be present but depth 3's children truncated
        let depth2 = &depth1["children"][0];
        assert_eq!(depth2["name"], "depth2");

        let depth3 = &depth2["children"][0];
        assert_eq!(depth3["name"], "depth3");

        // depth 3 children should be replaced with truncation marker
        let truncated = &depth3["children"];
        assert!(
            truncated
                .as_array()
                .map(|a| a.iter().any(|v| v.get("_truncated").is_some()))
                .unwrap_or(false),
            "depth 3 children should be truncated; got: {truncated}"
        );
    }

    #[test]
    fn truncate_dom_tree_leaves_shallow_tree_intact() {
        let mut tree = serde_json::json!({
            "name": "root",
            "children": [{"name": "child", "children": []}]
        });
        let original = tree.clone();
        truncate_dom_tree_depth(&mut tree, 3);
        assert_eq!(tree, original, "shallow tree should be unchanged");
    }

    #[test]
    fn truncate_dom_tree_caps_width_at_20() {
        // Build a node with 50 children
        let children: Vec<serde_json::Value> = (0..50)
            .map(|i| serde_json::json!({"name": format!("child{i}"), "children": []}))
            .collect();
        let mut tree = serde_json::json!({"name": "root", "children": children});

        truncate_dom_tree_depth(&mut tree, 3);

        let kept = tree["children"].as_array().unwrap();
        assert_eq!(
            kept.len(),
            21, // 20 real + 1 truncation marker
            "should keep 20 children + 1 truncation marker"
        );
        let marker = &kept[20];
        assert!(
            marker.get("_truncated").is_some(),
            "last entry should be truncation marker"
        );
        assert_eq!(
            marker["_omitted"],
            serde_json::json!(30),
            "should report 30 omitted children"
        );
    }

    #[test]
    fn build_node_virtual_root_retained_is_total_heap() {
        // 3 objects; obj0 dominates obj1 and obj2; virtual root = index 3.
        let n = 3usize;
        // Dominator-children CSR (len n+2): 0->[1,2], 1->[], 2->[], 3(vroot)->[0].
        let dc_off = vec![0u32, 2, 2, 2, 3];
        let dc_tgt = vec![1u32, 2, 0];
        let shallow = vec![10u32, 20, 30]; // total heap = 60
        let retained = vec![60u64, 20, 30]; // obj0 retains 60, obj1 20, obj2 30
        let class_idx = vec![0u32, 1, 2];
        let class_by_idx: std::collections::HashMap<u32, String> =
            [(0u32, "A"), (1, "B"), (2, "C")]
                .into_iter()
                .map(|(k, v)| (k, v.to_string()))
                .collect();

        let tree = browse_tree(
            3,
            3,
            10,
            n,
            &dc_off,
            &dc_tgt,
            &retained,
            &shallow,
            &class_idx,
            &class_by_idx,
        );

        assert_eq!(tree["class"], "<GC Root>");
        // The virtual GC root retains the entire heap: the sum of shallow sizes
        // (10+20+30 = 60). `retained.iter().sum()` double-counts dominated
        // objects (60+20+30 = 110), inflating the reported root size.
        assert_eq!(
            tree["retained_bytes"].as_u64(),
            Some(60),
            "virtual root retained_bytes must be total heap, got {tree}"
        );
    }

    #[test]
    fn truncate_dom_tree_depth_counts_wrapper_nesting() {
        // `get_report(leaks)` passes the WHOLE leaks report to
        // truncate_dom_tree_depth. The dominator_tree is nested under
        // `suspects[].dominator_tree`, so the depth counter starts at 2 rather
        // than 0 and the tree is capped two levels earlier than the documented
        // "capped at depth 3": only the root and its immediate children survive.
        let mut v = serde_json::json!({
            "suspects": [{
                "dominator_tree": {
                    "obj_index_1based": 1,
                    "children": [
                        {"obj_index_1based": 2, "children": [
                            {"obj_index_1based": 3, "children": [
                                {"obj_index_1based": 4, "children": []}
                            ]}
                        ]}
                    ]
                }
            }]
        });

        truncate_dom_tree_depth(&mut v, 3);

        let tree = &v["suspects"][0]["dominator_tree"];
        let level1 = &tree["children"][0];
        assert_eq!(level1["obj_index_1based"], 2);
        // The grandchild (level 3) must still be visible under a "depth 3" cap;
        // it is wiped today because the wrapper layers consume the depth budget.
        let level2 = &level1["children"][0];
        assert_eq!(
            level2["obj_index_1based"], 3,
            "grandchild should survive a depth-3 cap, got: {level1}"
        );
    }

    /// get_gc_roots logic: objects whose idom entry equals the virtual-root index (n).
    #[test]
    fn gc_roots_identified_by_idom_vroot() {
        // 3 objects; object 0 and 2 are GC roots (idom == 3 = n).
        // Object 1 is retained by object 0 (idom == 0).
        let n = 3usize;
        let vroot = n as u32;
        let idom: Vec<u32> = vec![vroot, 0, vroot, vroot]; // len n+1

        let roots: Vec<usize> = idom[..n]
            .iter()
            .enumerate()
            .filter(|(_, d)| **d == vroot)
            .map(|(i, _)| i)
            .collect();

        assert_eq!(roots, vec![0, 2], "objects 0 and 2 should be GC roots");
    }

    /// gc_roots limit is applied before building the result vector.
    #[test]
    fn gc_roots_limit_truncates_results() {
        let n = 5usize;
        let vroot = n as u32;
        let idom: Vec<u32> = vec![vroot; n + 1];

        let limit = 3usize;
        let roots: Vec<usize> = idom[..n]
            .iter()
            .enumerate()
            .filter(|(_, d)| **d == vroot)
            .take(limit)
            .map(|(i, _)| i)
            .collect();

        assert_eq!(roots.len(), limit);
        assert_eq!(roots, vec![0, 1, 2]);
    }

    /// field_name_pool round-trips through the cache write/read path.
    #[test]
    fn field_name_pool_cache_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let cache = crate::cache::CacheDir {
            path: dir.path().to_path_buf(),
        };

        let pool = vec![
            String::new(), // index 0 = unnamed sentinel
            "referenceQueue".to_string(),
            "next".to_string(),
            "value".to_string(),
        ];

        cache.write_field_name_pool(&pool).unwrap();
        let loaded = cache
            .read_field_name_pool()
            .unwrap()
            .expect("pool should be present");

        assert_eq!(loaded, pool);
    }

    /// read_field_name_pool returns None when the file does not exist.
    #[test]
    fn field_name_pool_missing_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let cache = crate::cache::CacheDir {
            path: dir.path().to_path_buf(),
        };
        let result = cache.read_field_name_pool().unwrap();
        assert!(result.is_none(), "should return None when pool file absent");
    }

    /// serialize_capped passes small payloads through unchanged.
    #[test]
    fn serialize_capped_small_payload_passthrough() {
        let val = serde_json::json!({"a": 1});
        let out = serialize_capped(&val, "hint").unwrap();
        assert!(out.contains("\"a\": 1"));
        assert!(!out.contains("_truncated"));
    }

    /// serialize_capped replaces oversized payloads with a truncation stub.
    #[test]
    fn serialize_capped_oversized_payload_becomes_stub() {
        let big = serde_json::json!({"blob": "x".repeat(300_000)});
        let out = serialize_capped(&big, "too big hint").unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed["_truncated"], serde_json::json!(true));
        assert!(parsed["_hint"].as_str().unwrap().contains("too big hint"));
        assert!(parsed["_bytes"].as_u64().unwrap() > 256_000);
    }

    /// build_node records total_children/children_truncated when the width cap
    /// drops siblings, and omits both on untruncated nodes.
    #[test]
    fn browse_node_reports_truncated_children() {
        // 4 objects, virtual root = index 4; vroot -> [0,1,2,3]; width cap = 2.
        // CSR convention (retained.rs build_dom_children_csr): offsets len n+2,
        // the vroot's children occupy row n = child_off[4]..child_off[5].
        let n = 4usize;
        let dc_off = vec![0u32, 0, 0, 0, 0, 4];
        let dc_tgt = vec![0u32, 1, 2, 3];
        let shallow = vec![10u32; n];
        let retained = vec![10u64; n];
        let class_idx = vec![0u32; n];
        let class_by_idx: std::collections::HashMap<u32, String> = [(0u32, "A")]
            .into_iter()
            .map(|(k, v)| (k, v.to_string()))
            .collect();

        let tree = browse_tree(
            4,
            1,
            2,
            n,
            &dc_off,
            &dc_tgt,
            &retained,
            &shallow,
            &class_idx,
            &class_by_idx,
        );

        assert_eq!(
            tree["total_children"].as_u64(),
            Some(4),
            "root must report 4 total children, got {tree}"
        );
        assert_eq!(
            tree["children_truncated"],
            serde_json::json!(true),
            "root with 4 children > width 2 must be marked truncated"
        );
        assert_eq!(
            tree["children"].as_array().unwrap().len(),
            2,
            "children must be capped at width"
        );
        // Leaf nodes did not descend (depth 1 exhausted): no markers emitted.
        let leaf = &tree["children"][0];
        assert!(leaf.get("total_children").is_none());
        assert!(leaf.get("children_truncated").is_none());
    }
}

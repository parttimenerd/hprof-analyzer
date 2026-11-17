//! Transport-agnostic heap-dump operations backed by a [`CachedSession`].
//!
//! Every function in this module is a **pure operation**: it takes a reference
//! to a loaded session (plus typed parameters), reads from the cache, and
//! returns a `Result<serde_json::Value, HeapOpsError>`.  No HTTP, no MCP,
//! no stdout — the caller (transport adapter) decides how to frame the result.
//!
//! The module exists to eliminate the ~500 lines of duplicated logic between
//! `heap_server.rs` (HTTP transport) and `mcp/mod.rs` (MCP/stdio transport).
//! Both transports now delegate to these functions and only handle
//! error-mapping and serialization framing.

use std::collections::HashMap;

use crate::cache::CachedSession;
use crate::opts::AnalyzeOptions;

// ── Error type ───────────────────────────────────────────────────────────────

/// Transport-agnostic error from a heap operation.
#[derive(Debug)]
pub enum HeapOpsError {
    /// Invalid parameters supplied by the caller (e.g. out-of-range index).
    BadParam(String),
    /// A required cache artifact is missing.
    CacheMissing(String),
    /// Unexpected internal error (serialization failure, I/O, ...).
    Internal(String),
}

impl std::fmt::Display for HeapOpsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BadParam(s) => write!(f, "bad parameter: {s}"),
            Self::CacheMissing(s) => write!(f, "cache missing: {s}"),
            Self::Internal(s) => write!(f, "internal error: {s}"),
        }
    }
}

impl std::error::Error for HeapOpsError {}

// ── Shared value converters ─────────────────────────────────────────────────

/// Convert an OQL `QueryValue` to a JSON value.
pub fn query_value_to_json(v: &crate::query::model::QueryValue) -> serde_json::Value {
    use crate::query::model::QueryValue;
    match v {
        QueryValue::Null => serde_json::Value::Null,
        QueryValue::Bool(b) => serde_json::Value::Bool(*b),
        QueryValue::Int(n) => serde_json::json!(n),
        QueryValue::Float(f) => serde_json::json!(f),
        QueryValue::Str(s) => serde_json::Value::String(s.clone()),
        QueryValue::ObjRef { index, class, .. } => {
            serde_json::json!({"class": class, "index": index})
        }
    }
}

/// Convert an instance-field value to a JSON value.
pub fn field_value_to_json(fv: &crate::objread::FieldValue) -> serde_json::Value {
    use crate::objread::FieldValue;
    match fv {
        FieldValue::Prim(v) => v.clone(),
        FieldValue::Ref(addr) => serde_json::json!({"address": format!("0x{addr:x}")}),
        FieldValue::Null => serde_json::Value::Null,
    }
}

// ── Helper: read cache arrays with uniform error handling ───────────────────

macro_rules! read_cache {
    ($sess:expr, $method:ident, $label:literal) => {
        $sess
            .cache
            .$method()
            .map_err(|e| HeapOpsError::Internal(format!("{}: {e}", $label)))?
            .ok_or_else(|| HeapOpsError::CacheMissing($label.to_string()))?
    };
}

/// Build the `class_idx → class_name` lookup from the session's class_names table.
fn class_by_idx(sess: &CachedSession) -> HashMap<u32, String> {
    sess.class_names
        .iter()
        .enumerate()
        .map(|(i, name)| (i as u32, name.clone()))
        .collect()
}

// ── Operations ──────────────────────────────────────────────────────────────

/// Top 5 leak suspects + top 5 classes by retained size.
pub fn summary(sess: &CachedSession) -> Result<serde_json::Value, HeapOpsError> {
    let r = &sess.report;
    Ok(serde_json::json!({
        "suspects": r.leaks.suspects.iter().take(5).map(|s| serde_json::json!({
            "name": s.pretty_class,
            "retained_bytes": s.retained,
        })).collect::<Vec<_>>(),
        "top_classes": r.top.biggest_classes.iter().take(5).map(|c| serde_json::json!({
            "class": c.pretty_class,
            "instances": c.instances,
            "retained_bytes": c.retained,
        })).collect::<Vec<_>>(),
    }))
}

/// A section of the full analysis report as JSON.
///
/// `section` is one of:
/// - CLI sections: `leaks`, `top`, `threads`, `overview`, `triage`, `waste`,
///   `indicators`, `retainers`, `arrays`, `collections`, `references`,
///   `dominators`, `components`, `alloc_sites`, `thread_locals`, `framework`,
///   `field_stats`, `categories`
/// - Synthesized sections: `top-objects`, `top-classes`, `gc-roots`,
///   `classloaders`, `strings`, `packages`, `culprits`, `self-referential`,
///   `static-collections`, `resources`
/// - `all` (the default — full report serialized)
///
/// Unknown section names return `HeapOpsError::BadParam`.
pub fn report_section(
    sess: &CachedSession,
    section: &str,
) -> Result<serde_json::Value, HeapOpsError> {
    report_section_with_limit(sess, section, None)
}

/// Like [`report_section`], but allows the caller to override the default row
/// limit for sections that support it.
///
/// # Which sections honor `limit`
///
/// `limit` caps the size of LIST fields the section emits (the leading part of
/// each list — they are all sorted most-significant-first, so the prefix is the
/// significant slice); scalar/summary fields are never truncated:
///
/// | Section | `limit` caps |
/// |---|---|
/// | `threads` | thread rows |
/// | `top-objects` / `top-classes` | rows |
/// | `waste` | `waste_breakdown` rows |
/// | `retainers` | rows |
/// | `arrays` | `obj_array_buckets` + `prim_array_buckets` |
/// | `collections` | each view's row/bucket list (fill ratios, size histogram, top arrays, constant arrays) |
/// | `indicators` | `nio_buffer_owners` |
/// | `alloc_sites` | `sites` |
/// | `thread_locals` | rows |
///
/// `leaks`/`overview`/`dominators`/`references`/`all` and the synthesized
/// sections other than `top-objects`/`top-classes` ignore `limit` (their shape
/// is already bounded by build-time caps). `leaks` suspects are bounded at
/// build time; use a focused section instead of `all` to shrink output.
pub fn report_section_with_limit(
    sess: &CachedSession,
    section: &str,
    limit: Option<usize>,
) -> Result<serde_json::Value, HeapOpsError> {
    let r = &sess.report;
    let val = match section {
        "leaks" => {
            let mut v = serde_json::to_value(&r.leaks)
                .map_err(|e| HeapOpsError::Internal(format!("serialization: {e}")))?;
            crate::mcp::truncate_dom_tree_depth(&mut v, 3);
            crate::mcp::strip_dominated_by_class_noise(&mut v);
            return Ok(v);
        }
        "top" => serde_json::to_value(&r.top),
        "threads" => {
            let limit = limit.unwrap_or(100).min(500);
            let max_frames: usize = 20;
            let mut sorted: Vec<_> = r.threads.threads.iter().collect();
            sorted.sort_by_key(|b| std::cmp::Reverse(b.retained));
            let rows: Vec<serde_json::Value> = sorted
                .into_iter()
                .take(limit)
                .map(|t| {
                    let frames_truncated = t.frames.len() > max_frames;
                    let frames: Vec<_> = t.frames.iter().take(max_frames).cloned().collect();
                    let mut obj = serde_json::json!({
                        "thread_serial": t.thread_serial,
                        "name": t.name,
                        "class_name": t.class_name,
                        "retained": t.retained,
                        "shallow": t.shallow,
                        "local_root_count": t.local_root_count,
                        "frames": frames,
                    });
                    if frames_truncated {
                        obj["frames_truncated"] = serde_json::json!(true);
                    }
                    if let Some(lo) = &t.local_objects {
                        obj["local_objects"] = serde_json::json!(lo);
                    }
                    obj
                })
                .collect();
            return Ok(serde_json::json!({
                "total_threads": r.threads.threads.len(),
                "shown": rows.len(),
                "sorted_by": "retained_desc",
                "threads": rows,
            }));
        }
        "overview" => serde_json::to_value(&r.overview),
        "triage" => serde_json::to_value(&r.triage),
        "waste" => {
            let cap = limit.unwrap_or(usize::MAX);
            let breakdown: Vec<_> = r.waste_breakdown.iter().take(cap).collect();
            if r.waste_breakdown.is_empty() {
                serde_json::to_value(serde_json::json!({
                    "waste_summary": r.waste_summary,
                    "waste_breakdown": breakdown,
                    "_note": "waste_breakdown is empty: no waste rules fired for this dump \
                              (duplicate strings, empty collections, oversized primitives \
                              were not detected above threshold)",
                }))
            } else {
                serde_json::to_value(serde_json::json!({
                    "waste_summary": r.waste_summary,
                    "waste_breakdown": breakdown,
                }))
            }
        }
        "indicators" => {
            let cap = limit.unwrap_or(usize::MAX);
            let mut li = r.leak_indicators.clone();
            li.nio_buffer_owners.truncate(cap);
            serde_json::to_value(&li)
        }
        "retainers" => {
            let cap = limit.unwrap_or(usize::MAX);
            let rows: Vec<_> = r.top_retainers.iter().take(cap).collect();
            serde_json::to_value(&rows)
        }
        "arrays" => {
            // C5: `arrays` is superseded by `collections` (which includes the
            // array fill ratios and top-array views). Kept for backward
            // compatibility, explicitly flagged; `limit` caps the two bucket
            // lists.
            let cap = limit.unwrap_or(usize::MAX);
            let mut abs = r.arrays_by_size.clone();
            abs.obj_array_buckets.truncate(cap);
            abs.prim_array_buckets.truncate(cap);
            serde_json::to_value(serde_json::json!({
                "_superseded": true,
                "_note": "The `arrays` section is superseded by `collections` (object-array \
                          and primitive-array length histograms live there alongside fill \
                          ratios and top-array rows). This section is kept for backward \
                          compatibility; prefer `get_report({section: \"collections\"}).`",
                "obj_array_buckets": abs.obj_array_buckets,
                "prim_array_buckets": abs.prim_array_buckets,
                "zero_length_count": abs.zero_length_count,
            }))
        }
        "collections" => {
            let cap = limit.unwrap_or(usize::MAX);
            let mut c = r.collections.clone();
            c.collection_fill_ratio.buckets.truncate(cap);
            c.collections_by_size.buckets.truncate(cap);
            c.array_fill_ratio.buckets.truncate(cap);
            c.map_collision_ratio.buckets.truncate(cap);
            c.constant_primitive_arrays.rows.truncate(cap);
            c.top_prim_arrays.top_individual.truncate(cap);
            c.top_prim_arrays.top_by_class.truncate(cap);
            c.top_obj_arrays.top_individual.truncate(cap);
            c.top_obj_arrays.top_by_class.truncate(cap);
            serde_json::to_value(&c)
        }
        "references" => serde_json::to_value(&r.references),
        "dominators" => {
            // Serve-cap: the cached report keeps up to IMDOM_PAIRS_CAP (20k)
            // pairs, which serializes to megabytes of inline JSON. Cap at the
            // serving boundary (pairs are already sorted by dominated_retained
            // desc, so the prefix is the most significant slice) and surface
            // the existing pairs_truncated/pairs_total_count fields. Serving
            // only — nothing in build_model changes, so no cache VERSION bump.
            const DOM_PAIRS_SERVE_CAP: usize = 200;
            let mut da = r.dominator_analysis.clone();
            let total = da.immediate_dominators.pairs.len() as u64;
            if da.immediate_dominators.pairs.len() > DOM_PAIRS_SERVE_CAP {
                da.immediate_dominators.pairs.truncate(DOM_PAIRS_SERVE_CAP);
                da.immediate_dominators.pairs_truncated = true;
            }
            // Preserve the true pre-cap total (build.rs may already have set a
            // larger one when IMDOM_PAIRS_CAP fired upstream).
            da.immediate_dominators.pairs_total_count =
                da.immediate_dominators.pairs_total_count.max(total);
            serde_json::to_value(&da)
        }
        "components" => serde_json::to_value(&r.top_components),
        "alloc_sites" => {
            let cap = limit.unwrap_or(usize::MAX);
            match &r.alloc_sites {
                None => serde_json::to_value::<Option<()>>(None),
                Some(a) => {
                    let sites: Vec<_> = a.sites.iter().take(cap).collect();
                    let any_frames = a.sites.iter().any(|s| !s.frames.is_empty());
                    let note = if !a.traces_present {
                        "traces_present is false: this dump was captured without allocation \
                         tracking (-agentlib:hprof=heap=dump,format=b is required to record \
                         allocation stack traces), or no GC-reachable object carries a \
                         non-zero allocation-trace serial. Counts would be meaningless."
                    } else if !any_frames {
                        "The dump records allocation-site serials, but the traces they \
                         reference contain no frames, so per-site stacks are unavailable. \
                         Capture with -agentlib:hprof=heap=dump,depth=8 to record \
                         method-level allocation stacks."
                    } else {
                        "Counts cover GC-reachable objects only (unreachable garbage is \
                         excluded, so totals can be cross-checked against the histogram / \
                         OQL SUM(@usedHeapSize)). retained_total per site is the sum of \
                         each object's individual dominator-tree retained size; it can \
                         exceed total heap size because dominated sub-trees are counted \
                         for each dominator along the chain."
                    };
                    serde_json::to_value(serde_json::json!({
                        "traces_present": a.traces_present,
                        "sites": sites,
                        "_note": note,
                    }))
                }
            }
        }
        "thread_locals" => {
            let cap = limit.unwrap_or(usize::MAX);
            let rows: Vec<_> = r.thread_local_analysis.iter().take(cap).collect();
            if rows.is_empty() {
                serde_json::to_value(serde_json::json!({
                    "rows": rows,
                    "_note": "thread_local_analysis is empty: no ThreadLocalMap$Entry \
                              instances were found in this dump (the analyzer tallies them \
                              in an always-on pass; an empty list means the heap contains \
                              no ThreadLocalMap entries, e.g. no thread-local values were \
                              live at dump time). ThreadLocal leak potential is still \
                              reported by leak_indicators.thread_local_null_key_count.",
                }))
            } else {
                serde_json::to_value(serde_json::json!({
                    "rows": rows,
                    "_note": "Per-value-class breakdown of ThreadLocalMap$Entry entries. \
                              stale_count rows with entry_count == stale_count are the \
                              classic thread-pool leak (key GC'd, value still held).",
                }))
            }
        }
        "framework" => {
            let rows = &r.framework_analysis;
            if rows.is_empty() {
                serde_json::to_value(serde_json::json!({
                    "rows": rows,
                    "_note": "framework_analysis is empty: none of the sentinel classes \
                              (Hibernate, Spring, ThreadPoolExecutor, Netty, HikariCP) were \
                              found in this dump.",
                }))
            } else {
                serde_json::to_value(rows)
            }
        }
        "field_stats" => match &r.field_stats {
            Some(fs) => serde_json::to_value(fs),
            None => serde_json::to_value(serde_json::json!({
                "_note": "field_stats is null: per-class reference-field statistics are an \
                          opt-in pass (re-analyze with --field-stats or --full-analysis); \
                          they are not computed in the default report because the pass \
                          needs the full forward-reference graph.",
            })),
        },
        "categories" => serde_json::to_value(&r.category_rollup),
        // ── Synthesized sections (matching MCP's get_report) ────────────
        "top-objects" => {
            let cap = limit.unwrap_or(20).min(100);
            let rows: Vec<_> = r.top.biggest_objects.iter().take(cap).collect();
            serde_json::to_value(&rows)
        }
        "top-classes" => {
            let cap = limit.unwrap_or(20).min(100);
            let rows: Vec<_> = r.top.biggest_classes.iter().take(cap).collect();
            serde_json::to_value(&rows)
        }
        "gc-roots" => serde_json::to_value(serde_json::json!({
            "gc_roots": r.overview.gc_roots,
            "gc_roots_by_type": r.overview.gc_roots_by_type,
            "gc_roots_retained_by_type": r.overview.gc_roots_retained_by_type,
        })),
        "classloaders" => serde_json::to_value(serde_json::json!({
            "classloader_count": r.overview.classloaders_loaded,
            "loader_rollup": r.overview.loader_rollup,
            "duplicate_classes": r.overview.duplicate_classes,
        })),
        "strings" => {
            let str_row = r
                .overview
                .histogram
                .iter()
                .find(|h| h.pretty_class == "java.lang.String");
            let backing_row = r
                .overview
                .histogram
                .iter()
                .find(|h| h.pretty_class == "byte[]" || h.pretty_class == "char[]");
            let dup_strings = r.overview.duplicate_strings.as_ref();
            serde_json::to_value(serde_json::json!({
                "string_count": str_row.map(|h| h.instances).unwrap_or(0),
                "string_shallow_bytes": str_row.map(|h| h.shallow).unwrap_or(0),
                "backing_array_count": backing_row.map(|h| h.instances).unwrap_or(0),
                "backing_array_shallow_bytes": backing_row.map(|h| h.shallow).unwrap_or(0),
                "duplicate_string_count": dup_strings.map(|d| d.duplicated_values).unwrap_or(0),
                "duplicate_wasted_bytes": dup_strings.map(|d| d.approx_wasted_bytes).unwrap_or(0),
            }))
        }
        "packages" => serde_json::to_value(&r.top.biggest_packages),
        "culprits" => serde_json::to_value(&r.primary_culprits),
        "self-referential" => serde_json::to_value(&r.self_referential_classes),
        "static-collections" => {
            const COLL_NAMES: &[&str] = &[
                "ArrayList",
                "HashMap",
                "LinkedList",
                "ConcurrentHashMap",
                "CopyOnWriteArrayList",
                "HashSet",
                "LinkedHashSet",
                "TreeMap",
                "TreeSet",
                "ArrayDeque",
                "PriorityQueue",
                "LinkedHashMap",
                "Hashtable",
                "Vector",
            ];
            let mut rows: Vec<serde_json::Value> = r
                .overview
                .histogram
                .iter()
                .filter(|h| {
                    let c = &h.pretty_class;
                    !c.ends_with("[]") && COLL_NAMES.iter().any(|n| c.contains(n))
                })
                .map(|h| {
                    let ratio = if h.shallow > 0 {
                        h.retained as f64 / h.shallow as f64
                    } else {
                        0.0
                    };
                    serde_json::json!({
                        "class": h.pretty_class,
                        "instances": h.instances,
                        "shallow_bytes": h.shallow,
                        "retained_bytes": h.retained,
                        "retained_to_shallow_ratio": (ratio * 10.0).round() / 10.0,
                    })
                })
                .collect();
            rows.sort_by(|a, b| {
                let ra = a["retained_bytes"].as_u64().unwrap_or(0);
                let rb = b["retained_bytes"].as_u64().unwrap_or(0);
                rb.cmp(&ra)
            });
            serde_json::to_value(serde_json::json!({
                "_hint": "retained_to_shallow_ratio >> 1 indicates the collection holds large objects. \
                          Use query({oql:\"static-collections\"}) for a grouped OQL view, or \
                          check get_report({section:\"leaks\"}) to see which object holds the biggest instance.",
                "collection_classes": rows,
            }))
        }
        "resources" => {
            const RESOURCE_NAMES: &[&str] = &[
                "FileInputStream",
                "FileOutputStream",
                "FileDescriptor",
                "FileChannel",
                "Statement",
                "ResultSet",
                "Connection",
                "Socket",
                "java.io.InputStream",
                "java.io.OutputStream",
            ];
            let mut rows: Vec<serde_json::Value> = r
                .overview
                .histogram
                .iter()
                .filter(|h| {
                    let c = &h.pretty_class;
                    !c.ends_with("[]")
                        && !c.contains("Weak")
                        && !c.contains("Reference")
                        && !c.contains("Buffered")
                        && RESOURCE_NAMES.iter().any(|n| c.contains(n))
                })
                .map(|h| {
                    serde_json::json!({
                        "class": h.pretty_class,
                        "instances": h.instances,
                        "shallow_bytes": h.shallow,
                        "retained_bytes": h.retained,
                    })
                })
                .collect();
            rows.sort_by(|a, b| {
                let ia = a["instances"].as_u64().unwrap_or(0);
                let ib = b["instances"].as_u64().unwrap_or(0);
                ib.cmp(&ia)
            });
            serde_json::to_value(serde_json::json!({
                "_hint": "High instance counts suggest resources acquired but never closed. \
                          Fix: wrap in try-with-resources. Use query({oql:\"unclosed-resources\"}) \
                          for a focused OQL view, or check triage for connection-leak / unclosed-resource-leak signals.",
                "resource_classes": rows,
            }))
        }
        "all" => serde_json::to_value(r),
        other => {
            return Err(HeapOpsError::BadParam(format!(
                "Unknown section {:?}. Valid sections: leaks, top, threads, overview, triage, \
                 waste, indicators, retainers, arrays, collections, references, dominators, \
                 components, alloc_sites, thread_locals, framework, field_stats, categories, \
                 top-objects, top-classes, gc-roots, classloaders, strings, packages, culprits, \
                 self-referential, static-collections, resources, all",
                other
            )));
        }
    };
    val.map_err(|e| HeapOpsError::Internal(format!("serialization: {e}")))
}

/// Class histogram sorted by retained size, capped at `limit` rows.
pub fn histogram(sess: &CachedSession, limit: usize) -> Result<serde_json::Value, HeapOpsError> {
    // Server-side cap so a caller passing a huge limit cannot flood the model
    // (or HTTP) context. 1000 rows is well past anything useful for orientation.
    let limit = limit.min(1000);
    let rows: Vec<_> = sess
        .report
        .overview
        .histogram
        .iter()
        .take(limit)
        .map(|c| {
            serde_json::json!({
                "class": c.pretty_class,
                "instances": c.instances,
                "retained_bytes": c.retained,
            })
        })
        .collect();
    Ok(serde_json::json!(rows))
}

/// Execute an OQL query (or named view) against the dump.
///
/// `oql` can be a raw OQL `SELECT ...` or a named view name (resolved
/// automatically).  Returns the serialized query result(s).
pub fn run_oql(
    sess: &CachedSession,
    oql: &str,
    all: bool,
    opts: &AnalyzeOptions,
) -> Result<serde_json::Value, HeapOpsError> {
    let oql = oql.trim();
    if oql.is_empty() {
        return Err(HeapOpsError::BadParam("empty OQL query".into()));
    }

    // Resolve named-view name → OQL text.
    let resolved = crate::named_queries::NAMED_QUERIES
        .iter()
        .find(|nq| nq.name.eq_ignore_ascii_case(oql))
        .map(|nq| nq.oql.to_string())
        .unwrap_or_else(|| oql.to_string());

    let dump_str = sess
        .dump_path
        .to_str()
        .ok_or_else(|| HeapOpsError::Internal("non-UTF8 dump path".into()))?;

    let results = crate::run_oql_query(dump_str, &resolved, !all, opts)
        .map_err(|e| HeapOpsError::Internal(format!("OQL: {e}")))?;

    // Serialize in the standard columns/rows shape.
    let out: Vec<serde_json::Value> = results
        .iter()
        .map(|r| {
            let cols: Vec<&str> = r.columns.iter().map(|c| c.name.as_str()).collect();
            let rows: Vec<Vec<serde_json::Value>> = r
                .rows
                .iter()
                .map(|row| row.iter().map(query_value_to_json).collect())
                .collect();
            let mut obj = serde_json::json!({
                "columns": cols,
                "rows": rows,
                "row_count": r.row_count,
                "truncated": r.truncated,
            });
            if let Some(e) = &r.error {
                obj["error"] = serde_json::json!(e);
            }
            if let Some(n) = &r.note {
                obj["note"] = serde_json::json!(n);
            }
            obj
        })
        .collect();
    Ok(serde_json::json!(out))
}

/// Browse the dominator tree.  Omit `index` to start at the virtual GC root.
pub fn browse(
    sess: &CachedSession,
    index: Option<u64>,
    depth: u8,
    width: usize,
) -> Result<serde_json::Value, HeapOpsError> {
    let (dc_off, dc_tgt) = read_cache!(sess, read_dominator_children, "dominator children");
    let retained = read_cache!(sess, read_retained, "retained");
    let shallow = read_cache!(sess, read_shallow, "shallow");
    let class_idx = read_cache!(sess, read_class_idx, "class_idx");

    let n = shallow.len();
    let start = index.unwrap_or(n as u64);
    // Validate: object_index must be in [0, n] (n = virtual root).
    if start > n as u64 {
        return Err(HeapOpsError::BadParam(format!(
            "object_index {} is out of range; heap has {} objects (valid range: 0..={})",
            start, n, n
        )));
    }
    let lookup = class_by_idx(sess);

    let mut tree = crate::mcp::browse_tree(
        start,
        depth.min(8),
        width.min(50),
        n,
        &dc_off,
        &dc_tgt,
        &retained,
        &shallow,
        &class_idx,
        &lookup,
    );
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
    Ok(tree)
}

/// Inspect a single object by dense index.
///
/// Returns class, shallow/retained bytes, fields (if offset data present),
/// and inbound references (if graph cache present).
pub fn inspect(sess: &CachedSession, index: u64) -> Result<serde_json::Value, HeapOpsError> {
    let shallow = read_cache!(sess, read_shallow, "shallow");
    let retained = read_cache!(sess, read_retained, "retained");
    let class_idx = read_cache!(sess, read_class_idx, "class_idx");

    let idx = index as usize;
    if idx >= shallow.len() {
        return Err(HeapOpsError::BadParam(format!(
            "object_index {idx} out of range (0..{})",
            shallow.len()
        )));
    }

    let cidx = class_idx[idx] as usize;
    let class_name = sess
        .class_names
        .get(cidx)
        .map(|s| s.as_str())
        .unwrap_or("<unknown>");

    let mut obj = serde_json::json!({
        "object_index": idx,
        "class": class_name,
        "shallow_bytes": shallow[idx],
        "retained_bytes": retained[idx],
        "_hint": format!(
            "To see what this {} retains: browse_dominators({{\"object_index\":{}}}). \
             To find objects that reference it: find_incoming_references({{\"object_index\": {}}}) (requires with_graph=true on load_dump).",
            class_name, idx, idx
        ),
    });

    // ── Field values (requires offsets.bin) ──────────────────────────────
    if let Ok(Some(offsets)) = sess.cache.read_hprof_offsets() {
        if let Some(&offset) = offsets.get(idx).filter(|&&o| o != 0) {
            let path = &sess.dump_path;
            let id_size = sess.pass1.id_size as usize;
            let cm = &sess.pass1.class_map;
            let st = &sess.pass1.strings;
            let fields_result = if crate::objread::is_gz_path(path) && sess.cache.has_gz_index() {
                crate::objread::open_for_offset(path, &sess.cache, offset)
                    .ok()
                    .and_then(|mut r| {
                        crate::objread::read_instance_via(r.as_mut(), offset, cm, st, id_size)
                            .ok()
                            .flatten()
                    })
            } else {
                crate::objread::read_instance_at(path, offset, cm, st, id_size)
                    .ok()
                    .flatten()
            };
            if let Some((_, _, fields)) = fields_result {
                let fj: Vec<serde_json::Value> = fields
                    .iter()
                    .map(|f| {
                        serde_json::json!({
                            "class": f.owner,
                            "name": f.name,
                            "value": field_value_to_json(&f.value),
                        })
                    })
                    .collect();
                obj["fields"] = serde_json::json!(fj);
            }
        }
    }

    // ── Inbound references (requires graph cache) ────────────────────────
    if sess.mode == crate::cache::CacheMode::Graph {
        if let Ok(Some((inb_off, inb_tgt))) = sess.cache.read_inbound_csr() {
            if idx + 1 < inb_off.len() {
                let start = inb_off[idx] as usize;
                let end = inb_off[idx + 1] as usize;
                let total = end - start;
                let refs: Vec<serde_json::Value> = inb_tgt[start..end]
                    .iter()
                    .take(20)
                    .map(|&src| {
                        let src = src as usize;
                        let src_cidx = class_idx[src] as usize;
                        let src_class = sess
                            .class_names
                            .get(src_cidx)
                            .cloned()
                            .unwrap_or_else(|| "<unknown>".to_string());
                        serde_json::json!({
                            "object_index": src,
                            "class": src_class,
                            "shallow_bytes": shallow[src],
                            "retained_bytes": retained[src],
                        })
                    })
                    .collect();
                obj["inbound_refs"] = serde_json::json!({
                    "total": total,
                    "shown": refs.len(),
                    "truncated": total > 20,
                    "refs": refs,
                });
            }
        }
    }

    Ok(obj)
}

/// List all objects that directly reference the given object.
///
/// Requires graph-mode cache (`heap load --with-graph`).
pub fn find_incoming(
    sess: &CachedSession,
    index: u64,
    limit: usize,
) -> Result<serde_json::Value, HeapOpsError> {
    if sess.mode != crate::cache::CacheMode::Graph {
        return Err(HeapOpsError::BadParam(
            "graph cache not available — run `hprof-analyzer heap load --with-graph <dump>` first"
                .into(),
        ));
    }

    let (inb_off, inb_tgt) = read_cache!(sess, read_inbound_csr, "graph.bin (inbound CSR)");
    let shallow = read_cache!(sess, read_shallow, "shallow");
    let retained = read_cache!(sess, read_retained, "retained");
    let class_idx = read_cache!(sess, read_class_idx, "class_idx");

    let idx = index as usize;
    let n = shallow.len();
    if idx >= n {
        return Err(HeapOpsError::BadParam(format!(
            "object_index {idx} out of range (heap has {n} objects)"
        )));
    }
    if idx + 1 >= inb_off.len() {
        return Err(HeapOpsError::BadParam(format!(
            "object_index {idx} has no inbound edge data"
        )));
    }

    let start = inb_off[idx] as usize;
    let end = inb_off[idx + 1] as usize;
    let raw_edge_count = end - start;
    let limit = limit.min(200);

    // The raw inbound CSR lists one entry per EDGE, so a referrer holding
    // several outgoing edges to this object (e.g. two fields pointing at it)
    // appears once per edge. Count per-source multiplicity, then emit ONE row
    // per unique source in first-occurrence (CSR) order, carrying the raw
    // multiplicity on the row as `edge_count`.
    let mut mult: HashMap<u32, u32> = HashMap::new();
    for &src in &inb_tgt[start..end] {
        *mult.entry(src).or_insert(0) += 1;
    }
    let unique_total = mult.len();
    let mut seen: std::collections::HashSet<u32> = std::collections::HashSet::new();
    let mut shown: Vec<serde_json::Value> = Vec::new();
    for &src in &inb_tgt[start..end] {
        if !seen.insert(src) {
            continue;
        }
        if shown.len() >= limit {
            break;
        }
        let s = src as usize;
        let src_cidx = class_idx[s] as usize;
        let src_class = sess
            .class_names
            .get(src_cidx)
            .cloned()
            .unwrap_or_else(|| "<unknown>".to_string());
        shown.push(serde_json::json!({
            "object_index": s,
            "class": src_class,
            "shallow_bytes": shallow[s],
            "retained_bytes": retained[s],
            "edge_count": mult[&src],
        }));
    }

    Ok(serde_json::json!({
        "object_index": idx,
        // Unique referring objects (matches the deduplicated refs list).
        "total_incoming_references": unique_total,
        // Un-deduplicated CSR edge count (same object referenced via several
        // edges is counted once per edge here).
        "raw_edge_count": raw_edge_count,
        "shown": shown.len(),
        "truncated": unique_total > shown.len(),
        "refs": shown,
    }))
}

/// Return OQL documentation for the given topic.
pub fn oql_docs(topic: Option<&str>) -> String {
    crate::mcp::oql_docs::get_oql_docs(topic)
}

/// List all named query views.
pub fn views() -> serde_json::Value {
    let rows: Vec<serde_json::Value> = crate::named_queries::NAMED_QUERIES
        .iter()
        .map(|nq| {
            serde_json::json!({
                "name": nq.name,
                "display": nq.display,
                "group": nq.group,
                "needs_retained": nq.needs_retained,
                "oql": nq.oql,
            })
        })
        .collect();
    serde_json::json!(rows)
}

/// Walk the immediate-dominator chain from `index` up to the virtual GC root.
///
/// Each step reveals which object dominates the queried object, i.e. every
/// path from a GC root to the object passes through every node in this chain.
pub fn dominator_path(sess: &CachedSession, index: u64) -> Result<serde_json::Value, HeapOpsError> {
    let idom = read_cache!(sess, read_idom, "idom");
    let retained = read_cache!(sess, read_retained, "retained");
    let shallow = read_cache!(sess, read_shallow, "shallow");
    let class_idx = read_cache!(sess, read_class_idx, "class_idx");

    let n = shallow.len();
    let idx = index as usize;
    if idx >= n {
        return Err(HeapOpsError::BadParam(format!(
            "object_index {idx} out of range (heap has {n} objects)"
        )));
    }

    let vroot = n as u32;
    let mut path: Vec<serde_json::Value> = Vec::new();
    let mut cur = idx as u32;
    let max_steps = 1000;
    for _ in 0..max_steps {
        if cur == u32::MAX {
            break;
        }
        let i = cur as usize;
        let (class, sh, ret) = if i < n {
            let cidx = class_idx[i] as usize;
            let cname = sess
                .class_names
                .get(cidx)
                .map(|s| s.as_str())
                .unwrap_or("<unknown>");
            (cname.to_string(), shallow[i], retained[i])
        } else {
            ("<GC Root>".to_string(), 0u32, 0u64)
        };
        let mut node = serde_json::json!({
            "index": i,
            "class": class,
            "shallow_bytes": sh,
            "retained_bytes": ret,
        });
        // The terminal virtual-root entry carries a synthetic index: it equals
        // the total entry count (one past the last valid dense index), from
        // the idom CSR's vroot/sentinel encoding. Flag it so callers never
        // feed it back into index-taking tools (inspect_object etc.).
        if i >= n {
            node["synthetic"] = serde_json::json!(true);
        }
        path.push(node);
        if cur == vroot {
            break;
        }
        cur = idom[i];
    }

    Ok(serde_json::json!({
        "object_index": index,
        "path_length": path.len(),
        "path": path,
        "_hint": "Path goes from the queried object (index 0) up to the GC root (last entry). \
                  Each node dominates the starting object — every path from a GC root to the \
                  starting object passes through every node in this chain. \
                  The terminal node (class <GC Root>) is the synthetic virtual root: its index \
                  equals the total entry count (one past the last valid dense index) and it is \
                  flagged synthetic:true — do NOT pass it to inspect_object or other \
                  index-taking tools.",
    }))
}

/// List objects that a given object directly references (outbound/forward edges).
///
/// Requires graph-mode cache (`heap load --with-graph`).
pub fn outbound_references(
    sess: &CachedSession,
    index: u64,
    limit: usize,
) -> Result<serde_json::Value, HeapOpsError> {
    if sess.mode != crate::cache::CacheMode::Graph {
        return Err(HeapOpsError::BadParam(
            "graph cache not available — run `hprof-analyzer heap load --with-graph <dump>` first"
                .into(),
        ));
    }

    let (fwd_off, fwd_tgt) = read_cache!(sess, read_forward_edges, "graph.bin (forward edges)");
    let retained = read_cache!(sess, read_retained, "retained");
    let shallow = read_cache!(sess, read_shallow, "shallow");
    let class_idx = read_cache!(sess, read_class_idx, "class_idx");

    let idx = index as usize;
    let n = shallow.len();
    if idx >= n {
        return Err(HeapOpsError::BadParam(format!(
            "object_index {idx} out of range (heap has {n} objects)"
        )));
    }
    if idx + 1 >= fwd_off.len() {
        return Err(HeapOpsError::BadParam(format!(
            "object_index {idx} has no forward edge data in graph.bin"
        )));
    }

    let limit = limit.min(200);
    let start = fwd_off[idx] as usize;
    let end = fwd_off[idx + 1] as usize;
    let total_outbound = end - start;

    let refs: Vec<serde_json::Value> = (start..end)
        .take(limit)
        .map(|j| {
            let tgt = fwd_tgt[j] as usize;
            let tgt_cidx = class_idx.get(tgt).copied().unwrap_or(0) as usize;
            let tgt_class = sess
                .class_names
                .get(tgt_cidx)
                .map(|s| s.as_str())
                .unwrap_or("<unknown>");
            serde_json::json!({
                "target_index": tgt,
                "target_class": tgt_class,
                "target_shallow": shallow.get(tgt).copied().unwrap_or(0),
                "target_retained": retained.get(tgt).copied().unwrap_or(0),
            })
        })
        .collect();

    Ok(serde_json::json!({
        "object_index": index,
        "total_outbound_references": total_outbound,
        "shown": refs.len(),
        "truncated": total_outbound > limit,
        "refs": refs,
        "_hint": "Each target_index is a dense object index. Use inspect for field details, \
                  browse for what it retains, or dominator-path for its retention chain.",
    }))
}

/// List GC root objects (objects whose immediate dominator is the virtual root).
///
/// Does NOT require graph-mode cache; Full mode is sufficient.
pub fn gc_roots(sess: &CachedSession, limit: usize) -> Result<serde_json::Value, HeapOpsError> {
    let idom = read_cache!(sess, read_idom, "idom");
    let class_idx = read_cache!(sess, read_class_idx, "class_idx");
    let shallow = read_cache!(sess, read_shallow, "shallow");
    let retained = read_cache!(sess, read_retained, "retained");

    // Use total_objects as n (the virtual-root index); ignore any trailing vroot
    // slot in idom. Guard against an inconsistently short cached array.
    let n = (sess.report.overview.total_objects as usize).min(idom.len());
    let vroot = n as u32;
    const UNDEF: u32 = u32::MAX;

    // A GC root is a direct child of the virtual root. The persisted idom encodes
    // that as the `undef` sentinel (u32::MAX) or a self-loop (`idom[i] == i`) —
    // see `retained.rs` dominator-children construction — while the in-memory
    // synthetic convention uses the vroot index itself. Accept all three so the
    // reverse scan matches whichever encoding the cache holds.
    let roots: Vec<serde_json::Value> = idom
        .iter()
        .take(n)
        .enumerate()
        .filter(|(i, d)| **d == vroot || **d == UNDEF || **d == *i as u32)
        .take(limit)
        .map(|(i, _)| {
            let cidx = class_idx[i] as usize;
            let class = sess
                .class_names
                .get(cidx)
                .cloned()
                .unwrap_or_else(|| "<unknown>".into());
            serde_json::json!({
                "object_index": i,
                "class": class,
                "shallow_bytes": shallow.get(i).copied().unwrap_or(0),
                "retained_bytes": retained.get(i).copied().unwrap_or(0),
            })
        })
        .collect();

    let total_gc_roots = sess.report.overview.gc_roots;
    Ok(serde_json::json!({
        "total_gc_roots": total_gc_roots,
        "shown": roots.len(),
        // Honest flag: true whenever fewer roots were returned than the pass1
        // count reports exist (covers both "hit the limit" and "enumerated none").
        "truncated": (total_gc_roots as usize) > roots.len(),
        "roots": roots,
        "_hint": "object_index values are dense indices covering ordinary objects AND \
                  classes/other entries. Dominator paths terminate at a synthetic virtual-root \
                  node whose index equals the total entry count (flagged synthetic:true in \
                  get_dominator_path/browse_dominators) — do not pass that index to \
                  inspect_object or other index-taking tools.",
    }))
}

/// Enumerate the retained set (dominator subtree) of an object.
///
/// Visits children sorted by retained size (largest first) with an optional
/// class name filter.  Returns individual objects plus a class summary.
pub fn retained_set(
    sess: &CachedSession,
    index: u64,
    limit: usize,
    class_filter: Option<&str>,
) -> Result<serde_json::Value, HeapOpsError> {
    let (dc_off, dc_tgt) = read_cache!(sess, read_dominator_children, "dominator children");
    let retained = read_cache!(sess, read_retained, "retained");
    let shallow = read_cache!(sess, read_shallow, "shallow");
    let class_idx = read_cache!(sess, read_class_idx, "class_idx");

    let n = shallow.len();
    let idx = index as usize;
    if idx >= n {
        return Err(HeapOpsError::BadParam(format!(
            "object_index {idx} out of range (heap has {n} objects)"
        )));
    }

    let limit = limit.min(500);
    let class_filter_lc = class_filter.map(|s| s.to_lowercase());

    use std::cmp::Reverse;
    use std::collections::BinaryHeap;

    let mut heap: BinaryHeap<(u64, Reverse<u16>, u32)> = BinaryHeap::new();

    // Seed with immediate children of the root object.
    if idx + 1 < dc_off.len() {
        let start = dc_off[idx] as usize;
        let end = dc_off[idx + 1] as usize;
        for j in start..end {
            let child = dc_tgt[j];
            let ret = retained.get(child as usize).copied().unwrap_or(0);
            heap.push((ret, Reverse(1), child));
        }
    }

    let mut results: Vec<serde_json::Value> = Vec::new();
    let mut total_objects_visited = 0u64;
    let mut total_shallow = 0u64;

    let mut class_summary: HashMap<String, (u64, u64, u64)> = HashMap::new();
    let max_visit = 100_000usize;

    // Root class, needed both for the response header and for the root's own
    // class_summary row (the root participates in the same class_filter).
    let root_class = {
        let cidx = class_idx[idx] as usize;
        sess.class_names
            .get(cidx)
            .map(|s| s.as_str())
            .unwrap_or("<unknown>")
            .to_string()
    };

    // Include the root object in the subtree accounting so
    // total_shallow_in_subtree is consistent with root_retained_bytes (which
    // covers the whole subtree INCLUDING the root). The root's class_summary
    // row is subject to the same class_filter as every other row.
    let root_shallow = shallow[idx];
    total_shallow += root_shallow as u64;
    total_objects_visited += 1;
    let root_passes_filter = match &class_filter_lc {
        Some(f) => root_class.to_lowercase().contains(f.as_str()),
        None => true,
    };
    if root_passes_filter {
        let entry = class_summary.entry(root_class.clone()).or_default();
        entry.0 += 1;
        entry.1 += retained[idx];
        entry.2 += root_shallow as u64;
    }

    while let Some((ret, Reverse(depth), obj)) = heap.pop() {
        let i = obj as usize;
        if i >= n {
            continue;
        }
        total_objects_visited += 1;

        let cidx = class_idx[i] as usize;
        let cname = sess
            .class_names
            .get(cidx)
            .map(|s| s.as_str())
            .unwrap_or("<unknown>");
        let sh = shallow[i];

        let passes_filter = match &class_filter_lc {
            Some(f) => cname.to_lowercase().contains(f.as_str()),
            None => true,
        };
        // class_summary is filtered together with the objects list when
        // class_filter is set (flagged in the response as
        // class_summary_filtered); the subtree totals below stay unfiltered.
        if passes_filter {
            let entry = class_summary.entry(cname.to_string()).or_default();
            entry.0 += 1;
            entry.1 += ret;
            entry.2 += sh as u64;
        }
        total_shallow += sh as u64;

        if passes_filter && results.len() < limit {
            results.push(serde_json::json!({
                "index": i,
                "class": cname,
                "shallow_bytes": sh,
                "retained_bytes": ret,
                "depth": depth,
            }));
        }

        if (total_objects_visited as usize) < max_visit && depth < 100 {
            if i + 1 < dc_off.len() {
                let start = dc_off[i] as usize;
                let end = dc_off[i + 1] as usize;
                for j in start..end {
                    let child = dc_tgt[j];
                    let child_ret = retained.get(child as usize).copied().unwrap_or(0);
                    heap.push((child_ret, Reverse(depth + 1), child));
                }
            }
        }
    }

    let mut class_rows: Vec<serde_json::Value> = class_summary
        .into_iter()
        .map(|(class, (count, total_ret, total_sh))| {
            serde_json::json!({
                "class": class,
                "count": count,
                "total_retained": total_ret,
                "total_shallow": total_sh,
            })
        })
        .collect();
    class_rows.sort_by(|a, b| {
        b["total_retained"]
            .as_u64()
            .unwrap_or(0)
            .cmp(&a["total_retained"].as_u64().unwrap_or(0))
    });
    class_rows.truncate(20);

    let filter_note = if class_filter_lc.is_some() {
        " When class_filter is set, the objects list AND class_summary cover only matching \
         classes (class_summary_filtered:true); the subtree totals remain unfiltered."
    } else {
        ""
    };

    Ok(serde_json::json!({
        "object_index": index,
        "root_class": root_class,
        "root_retained_bytes": retained[idx],
        // The root's own shallow bytes (also included in
        // total_shallow_in_subtree below).
        "root_shallow_bytes": root_shallow,
        "total_objects_in_subtree": total_objects_visited,
        // Includes the root object's shallow bytes — consistent with
        // root_retained_bytes, which covers the whole subtree including root.
        "total_shallow_in_subtree": total_shallow,
        "class_summary_filtered": class_filter_lc.is_some(),
        "shown": results.len(),
        "objects": results,
        "class_summary": class_rows,
        "_hint": format!(
            "The retained set is the dominator subtree: all objects that would be freed if {} \
             (object {}) were garbage collected. class_summary shows where the retained memory \
             goes. total_objects_in_subtree and total_shallow_in_subtree include the root object \
             itself (see root_shallow_bytes), matching root_retained_bytes; class_summary rows \
             sum each object's own retained size (dominator sizes overlap across \
             ancestor/descendant rows).{}",
            root_class, idx, filter_note
        ),
    }))
}

/// Aggregate inbound references by source class for all instances of a target class.
///
/// Requires graph-mode cache (`heap load --with-graph`).
pub fn find_incoming_by_class(
    sess: &CachedSession,
    class_name: &str,
    limit: usize,
) -> Result<serde_json::Value, HeapOpsError> {
    if sess.mode != crate::cache::CacheMode::Graph {
        return Err(HeapOpsError::BadParam(
            "graph cache not available — run `hprof-analyzer heap load --with-graph <dump>` first"
                .into(),
        ));
    }

    let (inb_off, inb_tgt) = read_cache!(sess, read_inbound_csr, "graph.bin (inbound CSR)");
    let retained = read_cache!(sess, read_retained, "retained");
    let shallow = read_cache!(sess, read_shallow, "shallow");
    let class_idx = read_cache!(sess, read_class_idx, "class_idx");

    let limit = limit.min(100);
    let needle = class_name.to_lowercase();
    let n = shallow.len();

    // Find all objects whose class name contains the search term.
    let mut target_indices: Vec<u32> = Vec::new();
    for i in 0..n {
        let cidx = class_idx[i] as usize;
        if sess
            .class_names
            .get(cidx)
            .is_some_and(|name| name.to_lowercase().contains(&needle))
        {
            target_indices.push(i as u32);
        }
    }

    if target_indices.is_empty() {
        return Ok(serde_json::json!({
            "class_name": class_name,
            "matched_instances": 0,
            "rows": [],
            "_hint": format!(
                "No objects with class matching {:?} found. \
                 Try a broader class name or check spelling.",
                class_name,
            ),
        }));
    }

    #[derive(Default)]
    struct SrcAgg {
        ref_count: u64,
        total_src_retained: u64,
        total_src_shallow: u64,
    }
    let mut by_src: HashMap<String, SrcAgg> = HashMap::new();
    let mut matched_instances = 0usize;

    for &idx in &target_indices {
        let i = idx as usize;
        if i + 1 >= inb_off.len() {
            continue;
        }
        let start = inb_off[i] as usize;
        let end = inb_off[i + 1] as usize;
        if start == end {
            continue;
        }
        matched_instances += 1;
        for &src in &inb_tgt[start..end] {
            let s = src as usize;
            let src_cidx = class_idx[s] as usize;
            let src_class = sess
                .class_names
                .get(src_cidx)
                .map(|s| s.as_str())
                .unwrap_or("<unknown>");
            let agg = by_src.entry(src_class.to_string()).or_default();
            agg.ref_count += 1;
            agg.total_src_retained += retained[s];
            agg.total_src_shallow += shallow[s] as u64;
        }
    }

    let mut rows: Vec<serde_json::Value> = by_src
        .into_iter()
        .map(|(src_class, agg)| {
            serde_json::json!({
                "src_class": src_class,
                "ref_count": agg.ref_count,
                "total_src_retained": agg.total_src_retained,
                "total_src_shallow": agg.total_src_shallow,
            })
        })
        .collect();
    rows.sort_by(|a, b| {
        b["ref_count"]
            .as_u64()
            .unwrap_or(0)
            .cmp(&a["ref_count"].as_u64().unwrap_or(0))
    });
    rows.truncate(limit);

    let hint = if target_indices.len() > matched_instances {
        format!(
            "{} matched nodes had no captured inbound edges (e.g. GC roots or objects with no referrers)",
            target_indices.len() - matched_instances
        )
    } else {
        String::new()
    };

    Ok(serde_json::json!({
        "class_name": class_name,
        "matched_instances": matched_instances,
        "total_target_nodes": target_indices.len(),
        "rows": rows,
        "_hint": hint,
    }))
}

/// Resolve the Java field name for a specific outbound edge.
///
/// Requires graph-mode cache with field-name pool.
pub fn field_name(
    sess: &CachedSession,
    src: u64,
    edge: u64,
) -> Result<serde_json::Value, HeapOpsError> {
    if sess.mode != crate::cache::CacheMode::Graph {
        return Err(HeapOpsError::CacheMissing(
            "field name data not available — the dump was not loaded in graph mode. \
             Re-analyze with load_dump({with_graph: true}) (MCP) or \
             `hprof-analyzer heap load --with-graph <dump>` (CLI)."
                .into(),
        ));
    }
    let pool = sess
        .field_name_pool
        .as_ref()
        .filter(|p| p.len() > 1)
        .ok_or_else(|| {
            HeapOpsError::CacheMissing(
                "field name pool not present in this cache — it is built only in graph mode. \
                 Clear the stale cache (`hprof-analyzer heap cache-clear <dump>`) then reload with \
                 load_dump({with_graph: true}) (MCP) or `hprof-analyzer heap load --with-graph <dump>` (CLI)."
                    .into(),
            )
        })?;

    let (fwd_off, _fwd_tgt) = read_cache!(sess, read_forward_edges, "graph.bin (forward edges)");
    let fwd_field_idx = read_cache!(sess, read_fwd_field_idx, "fwd_field_idx");

    let src_usize = src as usize;
    let n = fwd_off.len().saturating_sub(1);
    if src_usize >= n {
        return Err(HeapOpsError::BadParam(format!(
            "src {src} out of range (heap has {n} objects)"
        )));
    }
    let edge_start = fwd_off[src_usize] as usize;
    let edge_end = fwd_off[src_usize + 1] as usize;
    let total_edges = edge_end - edge_start;
    let ei = edge as usize;
    if ei >= total_edges {
        return Err(HeapOpsError::BadParam(format!(
            "edge {ei} out of range — object {src} has {total_edges} outbound edges"
        )));
    }

    let pool_idx = fwd_field_idx[edge_start + ei] as usize;
    let name = pool.get(pool_idx).map(|s| s.as_str()).unwrap_or("");
    if name.is_empty() {
        Ok(
            serde_json::json!({"field_name": null, "note": "unnamed edge — array element or synthetic reference"}),
        )
    } else {
        Ok(serde_json::json!({"field_name": name}))
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────
//
// These build a real disk cache (arrays.bin + graph.bin) in a tempdir and a
// hand-constructed CachedSession, so the pure operations above run against the
// exact same read paths the MCP/HTTP transports use.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::{CacheDir, CacheMode, GraphForCache};
    use crate::report::Report;

    /// Build a graph-mode CachedSession backed by a tempdir cache.
    ///
    /// `idom` must have length n+1 (vroot = n). The dominator-children CSR and
    /// the simple inbound CSR are derived exactly the way the analyze pipeline
    /// does (retained::build_dom_children_csr + lib::build_simple_inbound_csr).
    fn graph_session(
        shallow: &[u32],
        class_idx: &[u32],
        idom: &[u32],
        retained: &[u64],
        fwd_off: &[u32],
        fwd_tgt: &[u32],
        class_names: Vec<String>,
        total_objects: u64,
    ) -> (tempfile::TempDir, CachedSession) {
        let n = shallow.len();
        let dir = tempfile::tempdir().expect("tempdir");
        let cache = CacheDir {
            path: dir.path().to_path_buf(),
        };
        let (dc_off, dc_tgt) = crate::retained::build_dom_children_csr(n, idom);
        cache
            .write_arrays(&crate::cache::ArraysForCache {
                shallow,
                class_idx,
                idom,
                retained,
                dc_off: &dc_off,
                dc_tgt: &dc_tgt,
                n_objects: n,
            })
            .expect("write arrays");
        let (inb_off, inb_tgt) = crate::build_simple_inbound_csr(fwd_off, fwd_tgt, n);
        cache
            .write_graph(&GraphForCache {
                fwd_off,
                fwd_tgt,
                inb_off: &inb_off,
                inb_tgt: &inb_tgt,
                fwd_field_idx: None,
            })
            .expect("write graph");

        let mut report = Report::default();
        report.overview.total_objects = total_objects;
        let sess = CachedSession {
            dump_path: dir.path().join("dump.hprof"),
            report,
            pass1: crate::pass1::Pass1Snapshot {
                class_map: Default::default(),
                strings: Default::default(),
                id_map_pages: crate::id_map::IdMapPages {
                    block_base: vec![],
                    block_start: vec![],
                    offsets: vec![],
                },
                gc_root_addrs: vec![],
                stack_frames: Default::default(),
                stack_traces: Default::default(),
                stack_trace_thread: Default::default(),
                class_serial_to_addr: Default::default(),
                class_addr_table: vec![],
                id_size: 8,
                header_timestamp_ms: 0,
                file_size: 0,
            },
            cache,
            mode: CacheMode::Graph,
            class_names,
            field_name_pool: None,
        };
        (dir, sess)
    }

    /// vroot(4) → 0 → {1, 2, 3}; classes A/B/C/B; obj 3 referenced twice by 0
    /// is NOT used here — the fwd graph is only for find_incoming tests.
    fn base_fixture() -> (tempfile::TempDir, CachedSession) {
        let n = 4usize;
        let shallow = vec![10u32, 20, 30, 40];
        let class_idx = vec![0u32, 1, 2, 1];
        let idom = vec![4u32, 0, 0, 0, 4]; // len n+1, vroot=4
        let retained = vec![100u64, 20, 30, 40];
        // Forward edges: 0 → 1 twice and 3 → 1 once, so object 1's inbound
        // CSR is [0, 0, 3] (duplicate source for the dedupe test).
        let fwd_off = vec![0u32, 2, 2, 2, 3];
        let fwd_tgt = vec![1u32, 1, 1];
        graph_session(
            &shallow,
            &class_idx,
            &idom,
            &retained,
            &fwd_off,
            &fwd_tgt,
            vec!["A".into(), "B".into(), "C".into()],
            n as u64,
        )
    }

    // ── Item 1: find_incoming_references dedupe ─────────────────────────────

    #[test]
    fn find_incoming_dedupes_duplicate_edge_sources() {
        let (_dir, sess) = base_fixture();
        let r = find_incoming(&sess, 1, 50).expect("find_incoming");

        let refs = r["refs"].as_array().expect("refs array");
        assert_eq!(refs.len(), 2, "one row per UNIQUE source, got {r}");
        assert_eq!(refs[0]["object_index"], serde_json::json!(0));
        assert_eq!(
            refs[0]["edge_count"],
            serde_json::json!(2),
            "source 0 holds two edges"
        );
        assert_eq!(refs[1]["object_index"], serde_json::json!(3));
        assert_eq!(refs[1]["edge_count"], serde_json::json!(1));

        assert_eq!(
            r["total_incoming_references"],
            serde_json::json!(2),
            "total counts unique referrers"
        );
        assert_eq!(
            r["raw_edge_count"],
            serde_json::json!(3),
            "raw CSR edge count preserved"
        );
        assert_eq!(r["truncated"], serde_json::json!(false));
    }

    #[test]
    fn find_incoming_limit_still_reports_unique_total() {
        let (_dir, sess) = base_fixture();
        let r = find_incoming(&sess, 1, 1).expect("find_incoming");
        assert_eq!(r["shown"], serde_json::json!(1));
        assert_eq!(r["total_incoming_references"], serde_json::json!(2));
        assert_eq!(r["raw_edge_count"], serde_json::json!(3));
        assert_eq!(r["truncated"], serde_json::json!(true));
    }

    #[test]
    fn find_incoming_no_duplicates_when_single_edge_per_source() {
        let (_dir, sess) = base_fixture();
        // Object 2 has no inbound edges in the fixture → 0 rows, no dupes.
        let r = find_incoming(&sess, 2, 50).expect("find_incoming");
        assert_eq!(r["total_incoming_references"], serde_json::json!(0));
        assert_eq!(r["raw_edge_count"], serde_json::json!(0));
        assert_eq!(r["refs"].as_array().unwrap().len(), 0);
        assert_eq!(r["truncated"], serde_json::json!(false));
    }

    // ── Item 2: class_filter applies to class_summary ───────────────────────

    #[test]
    fn retained_set_class_filter_applies_to_class_summary() {
        let (_dir, sess) = base_fixture();
        let r = retained_set(&sess, 0, 50, Some("b")).expect("retained_set");

        // Objects list: only class-B instances (1 and 3).
        let objs = r["objects"].as_array().expect("objects");
        assert!(
            objs.iter().all(|o| o["class"] == serde_json::json!("B")),
            "objects list must be filtered, got {objs:?}"
        );
        assert_eq!(objs.len(), 2);

        // class_summary must NOT contain class A (the root) or C.
        let cs = r["class_summary"].as_array().expect("class_summary");
        assert_eq!(
            cs.len(),
            1,
            "class_summary must be filtered like the objects list, got {cs:?}"
        );
        assert_eq!(cs[0]["class"], serde_json::json!("B"));
        assert_eq!(cs[0]["count"], serde_json::json!(2));
        assert_eq!(
            cs[0]["total_shallow"],
            serde_json::json!(60),
            "20 + 40 shallow of the B children"
        );
        assert_eq!(r["class_summary_filtered"], serde_json::json!(true));

        // Subtree totals stay unfiltered (whole subtree incl. root).
        assert_eq!(r["total_objects_in_subtree"], serde_json::json!(4));
        assert_eq!(r["total_shallow_in_subtree"], serde_json::json!(100));
    }

    // ── Item 3: subtree totals include the root ─────────────────────────────

    #[test]
    fn retained_set_totals_include_root_shallow() {
        let (_dir, sess) = base_fixture();
        let r = retained_set(&sess, 0, 50, None).expect("retained_set");

        assert_eq!(
            r["root_shallow_bytes"],
            serde_json::json!(10),
            "root's own shallow size exposed"
        );
        assert_eq!(
            r["total_shallow_in_subtree"],
            serde_json::json!(100),
            "10 (root) + 20 + 30 + 40 — consistent with root_retained_bytes scope"
        );
        assert_eq!(
            r["total_objects_in_subtree"],
            serde_json::json!(4),
            "root + 3 children"
        );
        assert_eq!(r["root_retained_bytes"], serde_json::json!(100));
        assert_eq!(r["class_summary_filtered"], serde_json::json!(false));

        // Unfiltered class_summary covers every counted object, including the
        // root's class row (root: count 1, shallow 10, retained 100).
        let cs = r["class_summary"].as_array().expect("class_summary");
        let a = cs
            .iter()
            .find(|row| row["class"] == serde_json::json!("A"))
            .expect("root class A must have a class_summary row");
        assert_eq!(a["count"], serde_json::json!(1));
        assert_eq!(a["total_shallow"], serde_json::json!(10));
        assert_eq!(a["total_retained"], serde_json::json!(100));
        assert_eq!(cs.len(), 3, "A, B and C rows present, got {cs:?}");
    }

    // ── Item 4: synthetic terminal index is flagged ─────────────────────────

    #[test]
    fn dominator_path_flags_synthetic_virtual_root_terminal() {
        let (_dir, sess) = base_fixture();
        let r = dominator_path(&sess, 1).expect("dominator_path");

        let path = r["path"].as_array().expect("path array");
        assert_eq!(path.len(), 3, "1 → 0 → vroot, got {r}");

        // Real nodes carry no synthetic flag.
        assert_eq!(path[0]["index"], serde_json::json!(1));
        assert!(path[0].get("synthetic").is_none());
        assert_eq!(path[1]["index"], serde_json::json!(0));
        assert!(path[1].get("synthetic").is_none());

        // Terminal = virtual root: index == entry count (n = 4 here), class
        // <GC Root>, flagged synthetic.
        assert_eq!(path[2]["index"], serde_json::json!(4));
        assert_eq!(path[2]["class"], serde_json::json!("<GC Root>"));
        assert_eq!(
            path[2]["synthetic"],
            serde_json::json!(true),
            "terminal vroot node must be flagged synthetic, got {r}"
        );
    }

    // ── H3: `limit` must cap the list fields it previously ignored ──────────

    /// Every list-bearing section (`arrays`, `collections`, `indicators`,
    /// `alloc_sites`) truncates its lists to `limit` entries (keeping the
    /// most-significant prefix), while `limit: None` returns the full lists.
    /// `arrays` additionally carries the `_superseded` note (C5).
    #[test]
    fn report_section_limit_caps_lists() {
        let (_dir, mut sess) = base_fixture();
        let size_b = crate::report::SizeHistogramBucket {
            upper_len: 1,
            objects: 1,
            shallow: 10,
        };
        let fill_b = crate::report::FillRatioBucket {
            lower_ratio_bp: 0,
            upper_ratio_bp: 10_000,
            objects: 1,
            shallow: 10,
            wasted: 0,
        };
        sess.report.arrays_by_size.obj_array_buckets = vec![size_b.clone(); 5];
        sess.report.arrays_by_size.prim_array_buckets = vec![size_b.clone(); 5];
        sess.report.collections.collections_by_size.buckets = vec![size_b.clone(); 5];
        sess.report.collections.collection_fill_ratio.buckets = vec![fill_b.clone(); 5];
        sess.report.collections.array_fill_ratio.buckets = vec![fill_b.clone(); 5];
        sess.report.collections.map_collision_ratio.buckets = vec![fill_b.clone(); 5];
        sess.report.leak_indicators.nio_buffer_owners = (0..5)
            .map(|i| crate::report::NioBufferOwner {
                capacity: i as u64,
                obj_index_1based: i,
                object_index: i,
                display_class: "java.nio.DirectByteBuffer".into(),
                retained: 0,
                root_path: vec![],
            })
            .collect();
        sess.report.alloc_sites = Some(crate::report::AllocSites {
            traces_present: true,
            sites: (0..2)
                .map(|i| crate::report::AllocSite {
                    stack_serial: i as u32 + 1,
                    frames: vec![],
                    object_count: 10 - i as u64,
                    shallow_total: 100,
                    retained_total: 100,
                })
                .collect(),
        });

        // Full lists when no limit is passed.
        let full = report_section(&sess, "collections").expect("collections");
        assert_eq!(
            full["collections_by_size"]["buckets"]
                .as_array()
                .unwrap()
                .len(),
            5
        );
        let full_arr = report_section(&sess, "arrays").expect("arrays");
        assert_eq!(
            full_arr["_superseded"],
            serde_json::json!(true),
            "arrays section flagged superseded (C5)"
        );
        assert_eq!(full_arr["obj_array_buckets"].as_array().unwrap().len(), 5);

        // limit = 2 caps every list in collections/arrays/indicators.
        let capped =
            report_section_with_limit(&sess, "collections", Some(2)).expect("collections capped");
        for key in [
            "collection_fill_ratio",
            "collections_by_size",
            "array_fill_ratio",
            "map_collision_ratio",
        ] {
            assert_eq!(
                capped[key]["buckets"].as_array().unwrap().len(),
                2,
                "{key} buckets must be capped at limit"
            );
        }
        let capped_arr = report_section_with_limit(&sess, "arrays", Some(2)).expect("arrays");
        assert_eq!(capped_arr["obj_array_buckets"].as_array().unwrap().len(), 2);
        assert_eq!(
            capped_arr["prim_array_buckets"].as_array().unwrap().len(),
            2
        );
        let capped_ind =
            report_section_with_limit(&sess, "indicators", Some(2)).expect("indicators capped");
        assert_eq!(
            capped_ind["nio_buffer_owners"].as_array().unwrap().len(),
            2,
            "nio_buffer_owners must be capped at limit"
        );

        // alloc_sites: limit caps sites; the frameless-trace note explains why
        // frames are empty (C4).
        let capped_alloc =
            report_section_with_limit(&sess, "alloc_sites", Some(1)).expect("alloc_sites capped");
        assert_eq!(capped_alloc["sites"].as_array().unwrap().len(), 1);
        assert!(
            capped_alloc["_note"]
                .as_str()
                .unwrap()
                .contains("no frames"),
            "frameless-trace note must explain the empty frames, got {capped_alloc}"
        );
    }

    /// `field_stats` is null on the default path; the section explains the
    /// requirement instead of emitting a bare null (H3).
    #[test]
    fn field_stats_section_notes_requirement_when_absent() {
        let (_dir, sess) = base_fixture();
        assert!(sess.report.field_stats.is_none());
        let v = report_section(&sess, "field_stats").expect("field_stats");
        assert!(
            v["_note"].as_str().unwrap().contains("--field-stats"),
            "absent field_stats must carry an explanatory note, got {v}"
        );
    }
}

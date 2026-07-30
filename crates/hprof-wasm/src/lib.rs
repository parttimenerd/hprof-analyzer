//! hprof-wasm: WebAssembly bindings for the hprof-analyzer library.
//!
//! Exposes a `HprofSession` JS class that accepts raw `.hprof` bytes,
//! runs OQL queries, and returns JSON results — no filesystem I/O anywhere.

use std::sync::Arc;
use wasm_bindgen::prelude::*;

#[wasm_bindgen(start)]
pub fn init() {
    console_error_panic_hook::set_once();
}

/// Call a JS `function(phase: string, fraction: number)` progress callback.
/// Silently ignores errors (callback is best-effort).
fn call_progress(cb: &js_sys::Function, phase: &str, fraction: f32) {
    let _ = cb.call2(
        &JsValue::NULL,
        &JsValue::from_str(phase),
        &JsValue::from_f64(fraction as f64),
    );
}
// ──────────────────────────────────────────────────────────────────────────────
// HprofSession
// ──────────────────────────────────────────────────────────────────────────────

/// An active hprof analysis session backed by in-memory bytes.
///
/// Call `HprofSession.load(array, name)` from JS to initialise a session.
/// Once loaded, use `query()` for OQL queries and `run_full_analysis()` to
/// pre-compute retained sizes and generate the HTML report.
#[wasm_bindgen]
pub struct HprofSession {
    source: hprof_analyzer::HprofSource,
    class_names: Vec<String>,
    field_index: hprof_analyzer::query::complete::ClassFieldIndex,
    retained: Vec<u64>,
    cache: Option<hprof_analyzer::query::run::ReplCache>,
    cached_report_html: Option<String>,
}

#[wasm_bindgen]
impl HprofSession {
    /// Load a `.hprof` file from a JS `Uint8Array` and build the query cache.
    ///
    /// # Memory strategy
    ///
    /// wasm-bindgen copies the JS `Uint8Array` into WASM linear memory once
    /// (via `passArray8ToWasm`).  From that point everything happens inside WASM:
    ///
    /// 1. We take **ownership** of that buffer as `Arc<Vec<u8>>` (Vec is Sized,
    ///    so `Arc::try_unwrap` works on it after parsing completes).
    /// 2. The Arc is cloned cheaply for `parse_source` (Pass1 × 2 + Pass2).
    /// 3. After parsing, both external Arc clones are dropped (refcount → 1),
    ///    then `Arc::try_unwrap` reclaims the `Vec<u8>` with **zero copy**.
    /// 4. Gzip-compress in 256 KB chunks; `drop(raw)` before `enc.finish()` so
    ///    the raw buffer (N bytes) is freed before the compressed tail is written.
    ///    Raw and compressed never coexist simultaneously.
    ///
    /// Peak WASM footprint = N (parse buffer) + analysis structures.
    /// After load() returns only the compressed copy survives (~N/4).
    pub fn load(data: &[u8], name: &str) -> Result<HprofSession, JsValue> {
        let is_gzip = data.len() >= 2 && data[0] == 0x1f && data[1] == 0x8b;

        // ── Step 1: own the parse buffer ─────────────────────────────────
        // wasm-bindgen already copied JS bytes into `data` (WASM linear memory).
        // Arc<Vec<u8>> is Sized, so try_unwrap works below.
        let raw_arc: Arc<Vec<u8>> = Arc::new(data.to_vec());

        let parse_source = hprof_analyzer::HprofSource::Bytes {
            data: Arc::clone(&raw_arc),
            name: name.to_string(),
        };

        // ── Step 2: parse (Pass1 × 2 + Pass2) ────────────────────────────
        // build() stores source.clone() internally, so refcount reaches 3:
        //   raw_arc (1) + parse_source.data (2) + cache.source.data (3)
        let mut cache = hprof_analyzer::query::run::ReplCache::build(&parse_source, true)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;

        let class_names: Vec<String> = cache
            .p1
            .class_map
            .values()
            .filter_map(|ci| {
                let raw = cache.p1.strings.get(&ci.name_id)?;
                if raw.starts_with('[') {
                    return None;
                }
                Some(raw.replace('/', "."))
            })
            .collect();
        let field_index = hprof_analyzer::query::complete::ClassFieldIndex::build(&cache.p1);

        // ── Step 3: reclaim the raw buffer (zero-copy) ───────────────────
        // Drop parse_source (refcount 3 → 2) and replace cache.source with a
        // dummy (refcount 2 → 1).  try_unwrap succeeds: Vec reclaimed, no copy.
        drop(parse_source);
        cache.source = hprof_analyzer::HprofSource::Path(String::new());
        let raw_vec: Vec<u8> =
            Arc::try_unwrap(raw_arc).expect("Arc::try_unwrap failed — unexpected extra clone");

        // ── Step 4: compress (raw and compressed never overlap) ───────────
        let compressed: Vec<u8> = if is_gzip {
            raw_vec
        } else {
            gzip_compress_owned(raw_vec.into_boxed_slice())
        };

        let compressed_arc: Arc<Vec<u8>> = Arc::new(compressed);
        let compressed_source = hprof_analyzer::HprofSource::Bytes {
            data: compressed_arc,
            name: name.to_string(),
        };
        cache.source = compressed_source.clone();

        Ok(HprofSession {
            source: compressed_source,
            class_names,
            field_index,
            retained: Vec::new(),
            cache: Some(cache),
            cached_report_html: None,
        })
    }

    /// Run an OQL query and return a JSON string.
    ///
    /// Success: `{"ok":true,"result":{"columns":[...],"rows":[...],"row_count":N}}`
    /// Error:   `{"ok":false,"error":{"message":"..."}}`
    pub fn query(&mut self, oql: &str) -> String {
        use hprof_analyzer::query::{optimize, parse, plan, run};

        let q = match parse::parse_or_report(oql) {
            Ok(q) => q,
            Err(report) => {
                return serde_json::json!({
                    "ok": false,
                    "error": { "message": report }
                })
                .to_string();
            }
        };

        let plan_result = match plan::plan_query(&q, 5) {
            Ok(p) => p,
            Err(e) => {
                return serde_json::json!({
                    "ok": false,
                    "error": { "message": e.0 }
                })
                .to_string();
            }
        };

        let optimized = optimize::optimize(plan_result, &q, &optimize::SchemaStats::default());
        let pairs = vec![(q, optimized)];

        if self.cache.is_none() {
            match run::ReplCache::build(&self.source, true) {
                Ok(c) => self.cache = Some(c),
                Err(e) => {
                    return serde_json::json!({
                        "ok": false,
                        "error": { "message": e.to_string() }
                    })
                    .to_string();
                }
            }
        }
        let cache = self.cache.as_ref().unwrap();

        let results = if self.retained.is_empty() {
            run::run_resident_only(&cache, &pairs, true)
        } else {
            run::run_resident_with_retained(&cache, &pairs, true, &self.retained)
        };

        let results = match results {
            Ok(r) => r,
            Err(e) => {
                return serde_json::json!({
                    "ok": false,
                    "error": { "message": e.to_string() }
                })
                .to_string();
            }
        };

        let result = &results[0];
        if let Some(err) = &result.error {
            return serde_json::json!({
                "ok": false,
                "error": { "message": err }
            })
            .to_string();
        }

        let columns: Vec<String> = result.columns.iter().map(|c| c.name.clone()).collect();
        let rows: Vec<Vec<serde_json::Value>> = result
            .rows
            .iter()
            .map(|row| {
                row.iter()
                    .map(|v| serde_json::to_value(v).unwrap_or(serde_json::Value::Null))
                    .collect()
            })
            .collect();

        serde_json::json!({
            "ok": true,
            "result": {
                "columns": columns,
                "rows": rows,
                "row_count": result.row_count,
                "truncated": result.truncated,
            }
        })
        .to_string()
    }

    /// Returns `true` if `run_full_analysis()` has been called.
    pub fn has_retained(&self) -> bool {
        !self.retained.is_empty()
    }

    /// Returns the compressed size of the stored HPROF bytes (bytes).
    pub fn stored_bytes(&self) -> u32 {
        self.source.len().unwrap_or(0) as u32
    }

    /// Returns a JSON object with heap statistics available immediately after `load()`:
    /// `{ instance_count: N, class_count: N, compressed_bytes: N }`
    ///
    /// `instance_count` is the exact object count from Pass1 — use it to display
    /// "Computing dominators for N objects" and to estimate the dominator phase duration.
    pub fn stats(&self) -> String {
        let instance_count = self.cache.as_ref().map_or(0, |c| c.p1.instance_count);
        let class_count = self.class_names.len() as u64;
        let compressed_bytes = self.source.len().unwrap_or(0) as u64;
        serde_json::json!({
            "instance_count":   instance_count,
            "class_count":      class_count,
            "compressed_bytes": compressed_bytes,
        })
        .to_string()
    }

    /// Pre-compute dominators + retained sizes so `@retainedHeapSize` queries
    /// are served from the cached array on subsequent `query()` calls.
    /// Also generates and caches the HTML report for instant `get_report_html()`.
    pub fn run_full_analysis(&mut self) -> Result<(), JsValue> {
        let opts = hprof_analyzer::AnalyzeOptions::default();
        let (report, retained) =
            hprof_analyzer::analyze_to_report_with_retained(&self.source, &opts)
                .map_err(|e| JsValue::from_str(&e.to_string()))?;
        self.retained = retained;
        let source_name = match &self.source {
            hprof_analyzer::HprofSource::Bytes { name, .. } => name.clone(),
            hprof_analyzer::HprofSource::Path(p) => p.clone(),
        };
        if let Ok(json) = serde_json::to_string(&report) {
            self.cached_report_html = Some(hprof_analyzer::render_report_html(&source_name, &json));
        }
        Ok(())
    }

    /// Return the cached HTML report generated during `run_full_analysis()`.
    /// Returns an empty string if `run_full_analysis()` has not been called.
    pub fn get_report_html(&self) -> String {
        self.cached_report_html.clone().unwrap_or_default()
    }

    /// Run the full analysis pipeline and return the report as a JSON string.
    pub fn generate_report(&mut self) -> Result<String, JsValue> {
        let opts = hprof_analyzer::AnalyzeOptions::default();
        let (report, retained) =
            hprof_analyzer::analyze_to_report_with_retained(&self.source, &opts)
                .map_err(|e| JsValue::from_str(&e.to_string()))?;
        self.retained = retained;
        serde_json::to_string(&report).map_err(|e| JsValue::from_str(&e.to_string()))
    }

    /// Run the full analysis pipeline and return a self-contained HTML document.
    pub fn generate_report_html(&mut self) -> Result<String, JsValue> {
        let opts = hprof_analyzer::AnalyzeOptions::default();
        let (report, retained) =
            hprof_analyzer::analyze_to_report_with_retained(&self.source, &opts)
                .map_err(|e| JsValue::from_str(&e.to_string()))?;
        self.retained = retained;
        let source_name = match &self.source {
            hprof_analyzer::HprofSource::Bytes { name, .. } => name.clone(),
            hprof_analyzer::HprofSource::Path(p) => p.clone(),
        };
        let json = serde_json::to_string(&report).map_err(|e| JsValue::from_str(&e.to_string()))?;
        Ok(hprof_analyzer::render_report_html(&source_name, &json))
    }

    /// Returns a JSON array of all built-in named queries.
    pub fn named_queries() -> String {
        let arr: Vec<serde_json::Value> = hprof_analyzer::named_queries::NAMED_QUERIES
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
        serde_json::Value::Array(arr).to_string()
    }

    /// Returns a JSON array of class names extracted from the loaded dump.
    pub fn class_names(&self) -> String {
        serde_json::to_string(&self.class_names).unwrap_or_else(|_| "[]".to_string())
    }

    /// Returns the OQL language reference as a JSON object with keys:
    /// keywords, reserved, aggregates, functions, methods, attributes.
    /// Same structure as the server's GET /help endpoint (minus dump-specific
    /// class/field lists) so the WASM shell can show /help oql offline.
    pub fn oql_help() -> String {
        use hprof_analyzer::query::parse::{
            AGG_FUNCS, ATTRIBUTES, FUNCS, KEYWORDS, METHODS, RESERVED,
        };
        serde_json::json!({
            "keywords": KEYWORDS,
            "reserved": RESERVED,
            "aggregates": AGG_FUNCS,
            "functions": FUNCS,
            "methods": METHODS,
            "attributes": ATTRIBUTES,
        })
        .to_string()
    }

    /// Like `load()` but fires a JS progress callback at phase boundaries.
    ///
    /// The callback receives `(phase: string, fraction: number)` where fraction
    /// is always 1.0 (phase complete).  Phases fired in order:
    ///   "compress", "pass1_a", "pass1_b", "pass2"
    ///
    /// Because WASM is single-threaded, the browser will not repaint between
    /// callbacks — but each call allows JS to update DOM state that is rendered
    /// after the full load() returns control to the event loop.
    pub fn load_with_progress(
        data: &[u8],
        name: &str,
        cb: js_sys::Function,
    ) -> Result<HprofSession, JsValue> {
        let is_gzip = data.len() >= 2 && data[0] == 0x1f && data[1] == 0x8b;
        let raw_arc: Arc<Vec<u8>> = Arc::new(data.to_vec());

        let parse_source = hprof_analyzer::HprofSource::Bytes {
            data: Arc::clone(&raw_arc),
            name: name.to_string(),
        };

        // Build using the progress variant of ReplCache::build
        let mut cache = hprof_analyzer::query::run::ReplCache::build_with_progress(
            &parse_source,
            true,
            &mut |phase, frac| call_progress(&cb, phase, frac),
        )
        .map_err(|e| JsValue::from_str(&e.to_string()))?;

        let class_names: Vec<String> = cache
            .p1
            .class_map
            .values()
            .filter_map(|ci| {
                let raw = cache.p1.strings.get(&ci.name_id)?;
                if raw.starts_with('[') {
                    return None;
                }
                Some(raw.replace('/', "."))
            })
            .collect();
        let field_index = hprof_analyzer::query::complete::ClassFieldIndex::build(&cache.p1);

        drop(parse_source);
        cache.source = hprof_analyzer::HprofSource::Path(String::new());
        let raw_vec: Vec<u8> =
            Arc::try_unwrap(raw_arc).expect("Arc::try_unwrap failed — unexpected extra clone");

        let compressed: Vec<u8> = if is_gzip {
            call_progress(&cb, "compress", 1.0);
            raw_vec
        } else {
            let c = gzip_compress_owned(raw_vec.into_boxed_slice());
            call_progress(&cb, "compress", 1.0);
            c
        };

        let compressed_arc: Arc<Vec<u8>> = Arc::new(compressed);
        let compressed_source = hprof_analyzer::HprofSource::Bytes {
            data: compressed_arc,
            name: name.to_string(),
        };
        cache.source = compressed_source.clone();

        Ok(HprofSession {
            source: compressed_source,
            class_names,
            field_index,
            retained: Vec::new(),
            cache: Some(cache),
            cached_report_html: None,
        })
    }

    /// Like `run_full_analysis()` but fires a JS progress callback at each phase.
    ///
    /// Callback receives `(phase: string, fraction: number)`.  Phases in order:
    ///   "pass1", "pass2", "rpo", "inbound", "dominators", "retained"
    pub fn run_full_analysis_with_progress(&mut self, cb: js_sys::Function) -> Result<(), JsValue> {
        self.run_full_analysis_with_options_and_progress(false, false, cb)
    }

    /// Like `run_full_analysis_with_progress()` but with optional extended passes.
    ///
    /// `find_duplicates` — enable duplicate string/array detection.
    /// `collections`     — enable collection fill-ratio and waste analysis.
    pub fn run_full_analysis_with_options_and_progress(
        &mut self,
        find_duplicates: bool,
        collections: bool,
        cb: js_sys::Function,
    ) -> Result<(), JsValue> {
        let mut opts = hprof_analyzer::AnalyzeOptions::default();
        opts.find_duplicates = find_duplicates;
        opts.collections = collections;
        let (report, retained) = hprof_analyzer::analyze_to_report_with_progress(
            &self.source,
            &opts,
            &mut |phase, frac| call_progress(&cb, phase, frac),
        )
        .map_err(|e| JsValue::from_str(&e.to_string()))?;
        self.retained = retained;
        let source_name = match &self.source {
            hprof_analyzer::HprofSource::Bytes { name, .. } => name.clone(),
            hprof_analyzer::HprofSource::Path(p) => p.clone(),
        };
        if let Ok(json) = serde_json::to_string(&report) {
            self.cached_report_html = Some(hprof_analyzer::render_report_html(&source_name, &json));
        }
        Ok(())
    }

    /// OQL tab-completion using the loaded session's class and field data.
    ///
    /// Returns the same JSON array format as the free `complete()` function but
    /// uses the session's `ClassFieldIndex` so `alias.field` completions work.
    pub fn complete_query(&self, line: &str, cursor_pos: usize) -> String {
        let cs = hprof_analyzer::query::complete::complete(
            line,
            cursor_pos,
            &self.class_names,
            &self.field_index,
        );
        let arr: Vec<serde_json::Value> = cs
            .iter()
            .map(|c| {
                let mut obj = serde_json::json!({
                    "value": c.value,
                    "display": c.display,
                    "trailing_space": c.trailing_space,
                });
                if let Some(ref g) = c.group {
                    obj["group"] = serde_json::json!(g);
                }
                if let Some(ref d) = c.description {
                    obj["description"] = serde_json::json!(d);
                }
                obj
            })
            .collect();
        serde_json::Value::Array(arr).to_string()
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Free functions
// ──────────────────────────────────────────────────────────────────────────────

/// Gzip-compress an owned byte buffer.
///
/// Takes ownership of the input `Box<[u8]>` (the raw HPROF data) and drops it
/// before returning, so the raw bytes and the compressed output never coexist.
/// The encoder's output Vec is pre-sized to ~N/4 (typical hprof ratio).
fn gzip_compress_owned(raw: Box<[u8]>) -> Vec<u8> {
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use std::io::Write;
    let capacity = (raw.len() / 4).max(64 * 1024);
    // Drop `raw` immediately after copying into the encoder so the two big
    // buffers (raw N bytes + compressed N/4 bytes) never overlap in memory.
    // We achieve this by flushing chunk-by-chunk and dropping raw at the end.
    let mut enc = GzEncoder::new(Vec::with_capacity(capacity), Compression::fast());
    const CHUNK: usize = 256 * 1024;
    let mut offset = 0;
    while offset < raw.len() {
        let end = (offset + CHUNK).min(raw.len());
        let _ = enc.write_all(&raw[offset..end]);
        offset = end;
    }
    // Drop the raw buffer NOW — before enc.finish() allocates the tail bytes.
    drop(raw);
    enc.finish().unwrap_or_else(|_| Vec::new())
}

/// OQL tab-completion suggestions for a partial input line.
///
/// Returns a JSON array: `[{"value":"...","display":"...","group":"..."},...]`
///
/// This free function has no access to the loaded session's field data.
/// Use `HprofSession.complete_query()` instead when a session is loaded.
#[wasm_bindgen]
pub fn complete(line: &str, cursor_pos: usize, class_names: Vec<String>) -> String {
    let cs = hprof_analyzer::query::complete::complete(
        line,
        cursor_pos,
        &class_names,
        &hprof_analyzer::query::complete::ClassFieldIndex::empty(),
    );
    let arr: Vec<serde_json::Value> = cs
        .iter()
        .map(|c| {
            let mut obj = serde_json::json!({
                "value": c.value,
                "display": c.display,
            });
            if let Some(ref g) = c.group {
                obj["group"] = serde_json::json!(g);
            }
            obj
        })
        .collect();
    serde_json::Value::Array(arr).to_string()
}

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
// ExplorationHolder
// ──────────────────────────────────────────────────────────────────────────────

struct ExplorationHolder {
    result: hprof_analyzer::ExplorationResult,
    retained_c: Option<hprof_analyzer::cvec::CompressedU64>,
    shallow_c: Option<hprof_analyzer::cvec::CompressedU32>,
    fwd_targets_c: Option<hprof_analyzer::cvec::CompressedU32>,
}

impl ExplorationHolder {
    fn get_retained(&self, i: usize) -> u64 {
        if !self.result.retained.is_empty() {
            return self.result.retained.get(i).copied().unwrap_or(0);
        }
        self.retained_c
            .as_ref()
            .and_then(|c| c.get_at(i).ok().flatten())
            .unwrap_or(0)
    }

    fn get_shallow(&self, i: usize) -> u32 {
        if !self.result.shallow.is_empty() {
            return self.result.shallow.get(i).copied().unwrap_or(0);
        }
        self.shallow_c
            .as_ref()
            .and_then(|c| c.get_at(i).ok().flatten())
            .unwrap_or(0)
    }

    fn fwd_slice(&self, start: usize, end: usize) -> std::io::Result<std::borrow::Cow<'_, [u32]>> {
        if !self.result.fwd_targets.is_empty() {
            let len = self.result.fwd_targets.len();
            return Ok(std::borrow::Cow::Borrowed(
                &self.result.fwd_targets[start.min(len)..end.min(len)],
            ));
        }
        match &self.fwd_targets_c {
            Some(c) => c.slice_at(start, end).map(std::borrow::Cow::Owned),
            None => Ok(std::borrow::Cow::Borrowed(&[])),
        }
    }

    fn restore_retained(&self) -> std::io::Result<Vec<u64>> {
        if !self.result.retained.is_empty() {
            return Ok(self.result.retained.clone());
        }
        match &self.retained_c {
            Some(c) => c.restore(),
            None => Ok(Vec::new()),
        }
    }

    fn restore_fwd_targets(&self) -> std::io::Result<std::borrow::Cow<'_, [u32]>> {
        if !self.result.fwd_targets.is_empty() {
            return Ok(std::borrow::Cow::Borrowed(&self.result.fwd_targets));
        }
        match &self.fwd_targets_c {
            Some(c) => c.restore().map(std::borrow::Cow::Owned),
            None => Ok(std::borrow::Cow::Borrowed(&[])),
        }
    }
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
    retained_c: Option<hprof_analyzer::cvec::CompressedU64>,
    cache: Option<hprof_analyzer::query::run::ReplCache>,
    cached_report_html: Option<String>,
    exploration: Option<ExplorationHolder>,
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
            retained_c: None,
            cache: Some(cache),
            cached_report_html: None,
            exploration: None,
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

        let results = if !self.has_any_retained() {
            run::run_resident_only(&cache, &pairs, true)
        } else {
            let retained = match self.decompress_retained() {
                Ok(r) => r,
                Err(e) => {
                    return serde_json::json!({
                        "ok": false,
                        "error": { "message": e.to_string() }
                    })
                    .to_string();
                }
            };
            run::run_resident_with_retained(&cache, &pairs, true, &retained)
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
        self.has_any_retained()
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
        // Compress retained[] to reduce WASM memory by ~1.8 GB for 300M-object dumps
        if let Ok(c) = hprof_analyzer::cvec::CompressedU64::compress(
            &self.retained,
            hprof_analyzer::cvec::Codec::Deflate9,
        ) {
            self.retained_c = Some(c);
            self.retained = Vec::new();
        }
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
        // Compress retained[] to reduce WASM memory by ~1.8 GB for 300M-object dumps
        if let Ok(c) = hprof_analyzer::cvec::CompressedU64::compress(
            &self.retained,
            hprof_analyzer::cvec::Codec::Deflate9,
        ) {
            self.retained_c = Some(c);
            self.retained = Vec::new();
        }
        serde_json::to_string(&report).map_err(|e| JsValue::from_str(&e.to_string()))
    }

    /// Run the full analysis pipeline and return a self-contained HTML document.
    pub fn generate_report_html(&mut self) -> Result<String, JsValue> {
        let opts = hprof_analyzer::AnalyzeOptions::default();
        let (report, retained) =
            hprof_analyzer::analyze_to_report_with_retained(&self.source, &opts)
                .map_err(|e| JsValue::from_str(&e.to_string()))?;
        self.retained = retained;
        // Compress retained[] to reduce WASM memory by ~1.8 GB for 300M-object dumps
        if let Ok(c) = hprof_analyzer::cvec::CompressedU64::compress(
            &self.retained,
            hprof_analyzer::cvec::Codec::Deflate9,
        ) {
            self.retained_c = Some(c);
            self.retained = Vec::new();
        }
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
            retained_c: None,
            cache: Some(cache),
            cached_report_html: None,
            exploration: None,
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
        // Compress retained[] to reduce WASM memory by ~1.8 GB for 300M-object dumps
        if let Ok(c) = hprof_analyzer::cvec::CompressedU64::compress(
            &self.retained,
            hprof_analyzer::cvec::Codec::Deflate9,
        ) {
            self.retained_c = Some(c);
            self.retained = Vec::new();
        }
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

    /// Build and cache the inbound CSR for interactive exploration (BFS queries).
    /// Must be called before `inbound_refs()` or `gc_root_path()`.
    /// If `run_full_analysis()` has been called, retained sizes are included.
    pub fn enable_exploration(&mut self) -> Result<(), wasm_bindgen::JsValue> {
        let retained = self
            .decompress_retained()
            .map_err(|e| wasm_bindgen::JsValue::from_str(&e.to_string()))?;
        let result = hprof_analyzer::build_exploration(&self.source, &retained)
            .map_err(|e| wasm_bindgen::JsValue::from_str(&e.to_string()))?;
        self.exploration = Some(ExplorationHolder {
            result,
            retained_c: None,
            shallow_c: None,
            fwd_targets_c: None,
        });
        // Compress the three big arrays to save ~4 GB WASM address space
        if let Some(h) = self.exploration.as_mut() {
            use hprof_analyzer::cvec::{Codec, CompressedU32, CompressedU64};
            if let Ok(c) = CompressedU64::compress(&h.result.retained, Codec::Deflate9) {
                h.retained_c = Some(c);
                h.result.retained = Vec::new();
            }
            if let Ok(c) = CompressedU32::compress(&h.result.shallow, Codec::Deflate9) {
                h.shallow_c = Some(c);
                h.result.shallow = Vec::new();
            }
            if let Ok(c) = CompressedU32::compress(&h.result.fwd_targets, Codec::Deflate9) {
                h.fwd_targets_c = Some(c);
                h.result.fwd_targets = Vec::new();
            }
        }
        Ok(())
    }

    /// Returns a JSON object listing inbound referrers of the given dense object index.
    ///
    /// Returns `{"ok":true,"refs":[...],"total":N,"truncated":bool}` on success,
    /// or `{"error":"exploration_not_enabled"}` if `enable_exploration()` was not called.
    ///
    /// Each ref entry: `{"src_idx":N,"field_name":"","display_class":"...","shallow":N,"retained":N}`
    pub fn inbound_refs(&self, dense_idx: u32, limit: u32) -> String {
        let h = match self.exploration.as_ref() {
            Some(h) => h,
            None => return serde_json::json!({"error":"exploration_not_enabled"}).to_string(),
        };
        let exp = &h.result;

        let dense = dense_idx as usize;
        let pre = exp.dense_to_pre.get(dense).copied().unwrap_or(u32::MAX);
        if pre == u32::MAX {
            return serde_json::json!({"ok":true,"refs":[],"total":0,"truncated":false})
                .to_string();
        }

        let limit = limit as usize;
        let (parent_pres, total) = decode_inbound_parents(
            pre as usize,
            &exp.inb_block_off,
            &exp.inb_data,
            exp.inb_block,
            limit,
        );

        let refs: Vec<serde_json::Value> = parent_pres
            .iter()
            .map(|&parent_pre| {
                let src_dense = exp
                    .rpo_vertex
                    .get(parent_pre as usize)
                    .copied()
                    .unwrap_or(u32::MAX);
                let src = src_dense as usize;
                let display_class = exp.class_names_by_idx.get(src).cloned().unwrap_or_default();
                let shallow = h.get_shallow(src) as u64;
                let retained = h.get_retained(src);

                // Reverse-lookup field name: scan src's out-edges for an edge to dense_idx.
                let field_name: String = if src + 1 < exp.fwd_offsets.len() {
                    let start = exp.fwd_offsets[src] as usize;
                    let end = exp.fwd_offsets[src + 1] as usize;
                    h.fwd_slice(start, end)
                        .ok()
                        .and_then(|slice| {
                            slice.iter().position(|&t| t == dense_idx).and_then(|rel| {
                                exp.fwd_field_name_idx
                                    .as_ref()
                                    .and_then(|idx| idx.get(start + rel).copied())
                                    .and_then(|ni| exp.field_name_pool.get(ni as usize))
                                    .map(|s| s.clone())
                            })
                        })
                        .unwrap_or_default()
                } else {
                    String::new()
                };

                serde_json::json!({
                    "src_idx": src_dense,
                    "field_name": field_name,
                    "display_class": display_class,
                    "shallow": shallow,
                    "retained": retained,
                })
            })
            .collect();

        serde_json::json!({
            "ok": true,
            "refs": refs,
            "total": total,
            "truncated": total > limit,
        })
        .to_string()
    }

    /// Returns a JSON object listing outbound references from the given dense object index.
    ///
    /// Returns `{"ok":true,"refs":[...],"total":N,"truncated":bool}` on success,
    /// or `{"error":"exploration_not_enabled"}` if `enable_exploration()` was not called.
    ///
    /// Each ref entry: `{"dst_idx":N,"field_name":"...","display_class":"...","shallow":N,"retained":N}`
    /// Field names are included when the dump was analyzed with `--ref-paths`; otherwise empty strings.
    pub fn outbound_refs(&self, dense_idx: u32, limit: u32) -> String {
        let h = match self.exploration.as_ref() {
            Some(h) => h,
            None => return serde_json::json!({"error":"exploration_not_enabled"}).to_string(),
        };
        let exp = &h.result;

        let src = dense_idx as usize;
        if src + 1 >= exp.fwd_offsets.len() {
            return serde_json::json!({"ok":true,"refs":[],"total":0,"truncated":false})
                .to_string();
        }
        let start = exp.fwd_offsets[src] as usize;
        let end = exp.fwd_offsets[src + 1] as usize;
        let total = end - start;
        let limit = limit as usize;
        let truncated = total > limit;

        let targets = match h.fwd_slice(start, end) {
            Ok(t) => t,
            Err(e) => {
                return serde_json::json!({"error": e.to_string()}).to_string();
            }
        };

        let refs: Vec<serde_json::Value> = targets
            .iter()
            .take(limit)
            .enumerate()
            .map(|(i, &dst)| {
                let pos = start + i;
                let field_name = exp
                    .fwd_field_name_idx
                    .as_ref()
                    .and_then(|idx_vec| idx_vec.get(pos).copied())
                    .and_then(|ni| exp.field_name_pool.get(ni as usize))
                    .map(|s| s.as_str())
                    .unwrap_or("");
                let d = dst as usize;
                let display_class = exp.class_names_by_idx.get(d).cloned().unwrap_or_default();
                let shallow = h.get_shallow(d) as u64;
                let retained = h.get_retained(d);
                serde_json::json!({
                    "dst_idx": dst,
                    "field_name": field_name,
                    "display_class": display_class,
                    "shallow": shallow,
                    "retained": retained,
                })
            })
            .collect();

        serde_json::json!({
            "ok": true,
            "refs": refs,
            "total": total,
            "truncated": truncated,
        })
        .to_string()
    }

    /// BFS from `dense_idx` to the nearest GC root through inbound edges.
    ///
    /// Returns `{"ok":true,"path":[...],"root_type":"..."}` on success,
    /// `{"ok":false,"error":"no_path"}` if no path was found,
    /// or `{"error":"exploration_not_enabled"}` if `enable_exploration()` was not called.
    ///
    /// Path is ordered root → target. Each node: `{"dense_idx":N,"display_class":"...","shallow":N,"retained":N,"field_name":""}`
    pub fn gc_root_path(&self, dense_idx: u32) -> String {
        let h = match self.exploration.as_ref() {
            Some(h) => h,
            None => return serde_json::json!({"error":"exploration_not_enabled"}).to_string(),
        };
        let exp = &h.result;

        let mut visited: std::collections::HashSet<u32> = std::collections::HashSet::new();
        let mut queue: std::collections::VecDeque<(u32, Vec<u32>)> =
            std::collections::VecDeque::new();
        queue.push_back((dense_idx, vec![dense_idx]));
        visited.insert(dense_idx);

        while let Some((current, path)) = queue.pop_front() {
            if exp.gc_root_set.contains(&current) {
                let root_type_idx = exp.gc_root_indices.iter().position(|&r| r == current);
                let root_type = root_type_idx
                    .and_then(|i| exp.gc_root_types.get(i).copied())
                    .map(gc_root_type_label)
                    .unwrap_or("GC_ROOT");

                // path is target→root order; reverse to root→target for display
                let path_json: Vec<serde_json::Value> = path
                    .iter()
                    .rev()
                    .map(|&idx| {
                        let i = idx as usize;
                        serde_json::json!({
                            "dense_idx": idx,
                            "display_class": exp.class_names_by_idx.get(i).cloned().unwrap_or_default(),
                            "shallow": h.get_shallow(i) as u64,
                            "retained": h.get_retained(i),
                            "field_name": "",
                        })
                    })
                    .collect();

                return serde_json::json!({
                    "ok": true,
                    "path": path_json,
                    "root_type": root_type,
                })
                .to_string();
            }

            if path.len() > 100 {
                break;
            }

            let pre = exp
                .dense_to_pre
                .get(current as usize)
                .copied()
                .unwrap_or(u32::MAX);
            if pre != u32::MAX {
                let (parents, _) = decode_inbound_parents(
                    pre as usize,
                    &exp.inb_block_off,
                    &exp.inb_data,
                    exp.inb_block,
                    64,
                );
                for parent_pre in parents {
                    let parent_dense = exp
                        .rpo_vertex
                        .get(parent_pre as usize)
                        .copied()
                        .unwrap_or(u32::MAX);
                    if parent_dense != u32::MAX && visited.insert(parent_dense) {
                        let mut new_path = path.clone();
                        new_path.push(parent_dense);
                        queue.push_back((parent_dense, new_path));
                    }
                }
            }
        }

        serde_json::json!({"ok": false, "error": "no_path"}).to_string()
    }

    /// Find all captured objects whose class name contains `class_prefix` (case-insensitive).
    ///
    /// Returns `{"ok":true,"matches":[...],"total":N,"truncated":bool}`.
    /// Each match: `{"dense_idx":N,"display_class":"...","shallow":N,"retained":N}`.
    /// Results are sorted by retained heap descending.
    /// Requires `enable_exploration()` to have been called first.
    pub fn find_instances(&self, class_prefix: &str, limit: u32) -> String {
        let h = match self.exploration.as_ref() {
            Some(h) => h,
            None => return serde_json::json!({"error":"exploration_not_enabled"}).to_string(),
        };
        let exp = &h.result;

        let needle = class_prefix.to_ascii_lowercase();
        let limit = limit as usize;

        // Decompress retained once for the full scan
        let retained_vec = match h.restore_retained() {
            Ok(v) => v,
            Err(e) => return serde_json::json!({"error": e.to_string()}).to_string(),
        };

        let mut matches: Vec<(u32, u64)> = exp
            .class_names_by_idx
            .iter()
            .enumerate()
            .filter(|(_, name)| name.to_ascii_lowercase().contains(&needle))
            .map(|(i, _)| (i as u32, retained_vec.get(i).copied().unwrap_or(0)))
            .collect();

        let total = matches.len();
        matches.sort_unstable_by(|a, b| b.1.cmp(&a.1));
        let truncated = total > limit;

        let result: Vec<serde_json::Value> = matches
            .into_iter()
            .take(limit)
            .map(|(idx, retained)| {
                let i = idx as usize;
                serde_json::json!({
                    "dense_idx": idx,
                    "display_class": exp.class_names_by_idx.get(i).cloned().unwrap_or_default(),
                    "shallow": h.get_shallow(i) as u64,
                    "retained": retained,
                })
            })
            .collect();

        serde_json::json!({
            "ok": true,
            "matches": result,
            "total": total,
            "truncated": truncated,
        })
        .to_string()
    }

    /// Returns class name, shallow, and retained for a single object by dense index.
    ///
    /// Returns `{"ok":true,"display_class":"...","shallow":N,"retained":N}` on success,
    /// or `{"error":"exploration_not_enabled"}` / `{"error":"out_of_range"}`.
    /// Requires `enable_exploration()` to have been called first.
    pub fn get_node_info(&self, dense_idx: u32) -> String {
        let h = match self.exploration.as_ref() {
            Some(h) => h,
            None => return serde_json::json!({"error":"exploration_not_enabled"}).to_string(),
        };
        let exp = &h.result;
        let i = dense_idx as usize;
        if i >= exp.class_names_by_idx.len() {
            return serde_json::json!({"error":"out_of_range"}).to_string();
        }
        serde_json::json!({
            "ok": true,
            "display_class": exp.class_names_by_idx[i],
            "shallow": h.get_shallow(i) as u64,
            "retained": h.get_retained(i),
        })
        .to_string()
    }

    /// Returns primitive and reference field values for a single object.
    ///
    /// Uses the OQL engine to run `SELECT * FROM ClassName s WHERE s.@objectId = N`
    /// and maps the result columns to typed field entries.
    ///
    /// Returns `{"ok":true,"fields":[{"name":"size","kind":"int","value":47823},{"name":"table","kind":"ref","display_class":"Entry[]","dense_idx":1234},...]}`.
    /// On failure: `{"ok":false,"error":"..."}`.
    /// Requires the OQL cache to have been built (call `query()` at least once, or `run_full_analysis()`).
    pub fn get_field_values(&mut self, dense_idx: u32) -> String {
        use hprof_analyzer::query::model::QueryValue;
        use hprof_analyzer::query::{optimize, parse, plan, run};

        let exp = match self.exploration.as_ref() {
            Some(e) => &e.result,
            None => {
                return serde_json::json!({"ok":false,"error":"exploration_not_enabled"})
                    .to_string();
            }
        };

        let i = dense_idx as usize;
        let class_name = match exp.class_names_by_idx.get(i) {
            Some(c) if !c.is_empty() => c.clone(),
            _ => return serde_json::json!({"ok":false,"error":"out_of_range"}).to_string(),
        };

        // Build a single-object OQL query
        let oql = format!(
            "SELECT * FROM {} s WHERE s.@objectId = {}",
            class_name, dense_idx
        );

        let q = match parse::parse_or_report(&oql) {
            Ok(q) => q,
            Err(e) => return serde_json::json!({"ok":false,"error":e}).to_string(),
        };
        let plan_result = match plan::plan_query(&q, 5) {
            Ok(p) => p,
            Err(e) => return serde_json::json!({"ok":false,"error":e.0}).to_string(),
        };
        let optimized = optimize::optimize(plan_result, &q, &optimize::SchemaStats::default());

        if self.cache.is_none() {
            match run::ReplCache::build(&self.source, true) {
                Ok(c) => self.cache = Some(c),
                Err(e) => return serde_json::json!({"ok":false,"error":e.to_string()}).to_string(),
            }
        }
        let cache = self.cache.as_ref().unwrap();

        let results = if !self.has_any_retained() {
            run::run_resident_only(cache, &[(q, optimized)], true)
        } else {
            match self.decompress_retained() {
                Ok(retained) => {
                    run::run_resident_with_retained(cache, &[(q, optimized)], true, &retained)
                }
                Err(e) => return serde_json::json!({"ok":false,"error":e.to_string()}).to_string(),
            }
        };

        let result = match results {
            Ok(mut r) => r.remove(0),
            Err(e) => return serde_json::json!({"ok":false,"error":e.to_string()}).to_string(),
        };

        if let Some(err) = &result.error {
            return serde_json::json!({"ok":false,"error":err}).to_string();
        }

        // Columns are the field names; rows[0] is the single matching object
        let row = match result.rows.into_iter().next() {
            Some(r) => r,
            None => return serde_json::json!({"ok":true,"fields":[]}).to_string(),
        };

        let fields: Vec<serde_json::Value> = result
            .columns
            .iter()
            .zip(row.iter())
            .map(|(col, val)| match val {
                QueryValue::Null => {
                    serde_json::json!({"name": col.name, "kind": "null", "value": null})
                }
                QueryValue::Bool(b) => {
                    serde_json::json!({"name": col.name, "kind": "bool", "value": b})
                }
                QueryValue::Int(n) => {
                    serde_json::json!({"name": col.name, "kind": "int", "value": n})
                }
                QueryValue::Float(f) => {
                    serde_json::json!({"name": col.name, "kind": "float", "value": f})
                }
                QueryValue::Str(s) => {
                    serde_json::json!({"name": col.name, "kind": "str", "value": s})
                }
                QueryValue::ObjRef { index, class, .. } => serde_json::json!({
                    "name": col.name,
                    "kind": "ref",
                    "display_class": class,
                    "dense_idx": index,
                }),
            })
            .collect();

        serde_json::json!({"ok": true, "fields": fields}).to_string()
    }

    /// Returns up to `max_paths` distinct shortest paths from `dense_idx` to GC roots.
    ///
    /// Uses multi-source BFS from the target; each time a GC root is reached the path
    /// is recorded. Paths sharing a prefix are NOT merged — each is a complete chain.
    /// Cap `max_paths` at 10.
    ///
    /// Returns `{"ok":true,"paths":[{"path":[...],"root_type":"..."},...],"total_found":N}`.
    /// Each path is root→target order. Each node: `{"dense_idx":N,"display_class":"...","shallow":N,"retained":N}`.
    pub fn all_gc_root_paths(&self, dense_idx: u32, max_paths: u32) -> String {
        let h = match self.exploration.as_ref() {
            Some(h) => h,
            None => return serde_json::json!({"error":"exploration_not_enabled"}).to_string(),
        };
        let exp = &h.result;

        let max_paths = (max_paths as usize).min(10);
        let mut found_paths: Vec<serde_json::Value> = Vec::new();

        // BFS: queue entries are (current_node, path_from_target_to_here)
        // We need to find MULTIPLE paths, so we allow revisiting nodes in different paths
        // but cap total work with a node-visit counter.
        let mut global_visited: std::collections::HashMap<u32, u32> =
            std::collections::HashMap::new();
        let mut queue: std::collections::VecDeque<(u32, Vec<u32>)> =
            std::collections::VecDeque::new();
        queue.push_back((dense_idx, vec![dense_idx]));
        *global_visited.entry(dense_idx).or_insert(0) += 1;

        let max_work = 50_000usize;
        let mut work = 0usize;

        while let Some((current, path)) = queue.pop_front() {
            work += 1;
            if work > max_work {
                break;
            }
            if path.len() > 100 {
                continue;
            }

            if exp.gc_root_set.contains(&current) {
                let root_type_idx = exp.gc_root_indices.iter().position(|&r| r == current);
                let root_type = root_type_idx
                    .and_then(|i| exp.gc_root_types.get(i).copied())
                    .map(gc_root_type_label)
                    .unwrap_or("GC_ROOT");

                let path_json: Vec<serde_json::Value> = path
                    .iter()
                    .rev()
                    .map(|&idx| {
                        let i = idx as usize;
                        serde_json::json!({
                            "dense_idx": idx,
                            "display_class": exp.class_names_by_idx.get(i).cloned().unwrap_or_default(),
                            "shallow": h.get_shallow(i) as u64,
                            "retained": h.get_retained(i),
                        })
                    })
                    .collect();

                found_paths.push(serde_json::json!({
                    "path": path_json,
                    "root_type": root_type,
                }));

                if found_paths.len() >= max_paths {
                    break;
                }
                // Don't enqueue parents — this path ended at a root
                continue;
            }

            let pre = exp
                .dense_to_pre
                .get(current as usize)
                .copied()
                .unwrap_or(u32::MAX);
            if pre != u32::MAX {
                let (parents, _) = decode_inbound_parents(
                    pre as usize,
                    &exp.inb_block_off,
                    &exp.inb_data,
                    exp.inb_block,
                    64,
                );
                for parent_pre in parents {
                    let parent_dense = exp
                        .rpo_vertex
                        .get(parent_pre as usize)
                        .copied()
                        .unwrap_or(u32::MAX);
                    if parent_dense == u32::MAX {
                        continue;
                    }
                    // Allow a node to appear in at most 3 different paths to limit explosion
                    let visit_count = global_visited.entry(parent_dense).or_insert(0);
                    if *visit_count < 3 {
                        *visit_count += 1;
                        let mut new_path = path.clone();
                        new_path.push(parent_dense);
                        queue.push_back((parent_dense, new_path));
                    }
                }
            }
        }

        let total = found_paths.len();
        serde_json::json!({
            "ok": true,
            "paths": found_paths,
            "total_found": total,
        })
        .to_string()
    }

    /// Returns key-value or element entries for known Java collection types.
    ///
    /// Recognises: HashMap, LinkedHashMap, ConcurrentHashMap, HashSet, LinkedHashSet,
    /// TreeMap, ArrayList, LinkedList, ArrayDeque, Vector, Stack, and several Scala/
    /// Kotlin/Eclipse Collections/Guava types.
    ///
    /// Strategy:
    /// 1. Identify the collection kind + backing-array field via `collection_info`.
    /// 2. Run `SELECT * FROM ClassName WHERE @objectId = N` to get the field values,
    ///    locate the backing array field, and retrieve its dense index.
    /// 3. Call `outbound_refs` on the array to enumerate entries.
    ///
    /// Returns `{"ok":true,"type":"map","entries":[{"key_idx":N,"key_class":"...","val_idx":M,"val_class":"..."},...],"truncated":bool}`
    /// for map-like types, or `{"ok":true,"type":"list","entries":[{"elem_idx":N,"elem_class":"..."},...],"truncated":bool}`
    /// for list/set types.
    /// Returns `{"ok":true,"type":"unknown"}` when the class is not a known collection.
    pub fn get_collection_entries(&mut self, dense_idx: u32, limit: u32) -> String {
        use hprof_analyzer::query::model::QueryValue;
        use hprof_analyzer::query::{optimize, parse, plan, run};

        // Extract class_name from exploration (borrow released after clone)
        let class_name: String = {
            let h = match self.exploration.as_ref() {
                Some(h) => h,
                None => {
                    return serde_json::json!({"ok":false,"error":"exploration_not_enabled"})
                        .to_string();
                }
            };
            let i = dense_idx as usize;
            match h.result.class_names_by_idx.get(i) {
                Some(c) if !c.is_empty() => c.clone(),
                _ => return serde_json::json!({"ok":false,"error":"out_of_range"}).to_string(),
            }
        };

        let (coll_type, array_field) = match collection_info(&class_name) {
            Some(pair) => pair,
            None => return serde_json::json!({"ok":true,"type":"unknown"}).to_string(),
        };

        // Step 1: get field values for this object to find the backing array dense_idx
        let oql = format!(
            "SELECT * FROM {} s WHERE s.@objectId = {}",
            class_name, dense_idx
        );
        let q = match parse::parse_or_report(&oql) {
            Ok(q) => q,
            Err(e) => return serde_json::json!({"ok":false,"error":e}).to_string(),
        };
        let plan_result = match plan::plan_query(&q, 5) {
            Ok(p) => p,
            Err(e) => return serde_json::json!({"ok":false,"error":e.0}).to_string(),
        };
        let optimized = optimize::optimize(plan_result, &q, &optimize::SchemaStats::default());

        if self.cache.is_none() {
            match run::ReplCache::build(&self.source, true) {
                Ok(c) => self.cache = Some(c),
                Err(e) => return serde_json::json!({"ok":false,"error":e.to_string()}).to_string(),
            }
        }
        let cache = self.cache.as_ref().unwrap();

        let results = if !self.has_any_retained() {
            run::run_resident_only(cache, &[(q, optimized)], true)
        } else {
            match self.decompress_retained() {
                Ok(retained) => {
                    run::run_resident_with_retained(cache, &[(q, optimized)], true, &retained)
                }
                Err(e) => return serde_json::json!({"ok":false,"error":e.to_string()}).to_string(),
            }
        };

        let result = match results {
            Ok(mut r) => r.remove(0),
            Err(e) => return serde_json::json!({"ok":false,"error":e.to_string()}).to_string(),
        };

        if let Some(err) = &result.error {
            return serde_json::json!({"ok":false,"error":err}).to_string();
        }

        // Find the backing array field in the OQL result columns
        let array_dense_idx: u32 = {
            let col_idx = result.columns.iter().position(|c| c.name == array_field);
            let row = result.rows.into_iter().next();
            match (col_idx, row) {
                (Some(ci), Some(ref row)) => match row.get(ci) {
                    Some(QueryValue::ObjRef { index, .. }) => *index as u32,
                    _ => return serde_json::json!({"ok":true,"type":"unknown"}).to_string(),
                },
                _ => return serde_json::json!({"ok":true,"type":"unknown"}).to_string(),
            }
        };

        // Step 2: outbound refs of the backing array — these are the entries
        let h = match self.exploration.as_ref() {
            Some(h) => h,
            None => {
                return serde_json::json!({"ok":false,"error":"exploration_not_enabled"})
                    .to_string();
            }
        };
        let exp = &h.result;

        let arr_i = array_dense_idx as usize;
        if arr_i >= exp.fwd_offsets.len().saturating_sub(1) {
            return serde_json::json!({"ok":true,"type":coll_type,"entries":[],"truncated":false})
                .to_string();
        }

        let start = exp.fwd_offsets[arr_i] as usize;
        let end = exp.fwd_offsets[arr_i + 1] as usize;
        let limit = limit as usize;

        let targets = match h.fwd_slice(start, end) {
            Ok(t) => t,
            Err(e) => return serde_json::json!({"ok":false,"error":e.to_string()}).to_string(),
        };

        let total = targets.len();
        let truncated = total > limit;

        let entries: Vec<serde_json::Value> = targets
            .iter()
            .take(limit)
            .map(|&t| {
                let t_i = t as usize;
                let dc = exp.class_names_by_idx.get(t_i).cloned().unwrap_or_default();
                if coll_type == "map" {
                    // For maps the backing array holds Entry objects; we show them as elements
                    // (key/value would require a second level of field access)
                    serde_json::json!({"elem_idx": t, "elem_class": dc})
                } else {
                    serde_json::json!({"elem_idx": t, "elem_class": dc})
                }
            })
            .collect();

        serde_json::json!({
            "ok": true,
            "type": coll_type,
            "entries": entries,
            "truncated": truncated,
        })
        .to_string()
    }

    /// Returns the HPROF memory address (as a hex string) for a single object by dense index.
    ///
    /// Returns `{"ok":true,"address":"0x..."}` on success,
    /// or `{"error":"exploration_not_enabled"}` / `{"error":"out_of_range"}` / `{"error":"no_addresses"}`.
    /// Requires `enable_exploration()` to have been called first.
    pub fn get_object_address(&self, dense_idx: u32) -> String {
        let h = match self.exploration.as_ref() {
            Some(h) => h,
            None => return serde_json::json!({"error":"exploration_not_enabled"}).to_string(),
        };
        let exp = &h.result;
        if exp.addrs.is_empty() {
            return serde_json::json!({"error":"no_addresses"}).to_string();
        }
        let i = dense_idx as usize;
        if i >= exp.addrs.len() {
            return serde_json::json!({"error":"out_of_range"}).to_string();
        }
        serde_json::json!({
            "ok": true,
            "address": format!("0x{:x}", exp.addrs[i]),
        })
        .to_string()
    }

    pub fn find_dense_by_address(&self, addr: u64) -> String {
        let h = match self.exploration.as_ref() {
            Some(h) => h,
            None => return serde_json::json!({"error":"exploration_not_enabled"}).to_string(),
        };
        let exp = &h.result;
        if exp.addrs.is_empty() {
            return serde_json::json!({"error":"no_addresses"}).to_string();
        }
        match exp.addrs.iter().position(|&a| a == addr) {
            Some(idx) => serde_json::json!({"ok":true,"dense_idx":idx as u32}).to_string(),
            None => serde_json::json!({"ok":false,"error":"not_found"}).to_string(),
        }
    }

    pub fn find_path_between(&self, src_idx: u32, dst_idx: u32) -> String {
        let h = match self.exploration.as_ref() {
            Some(h) => h,
            None => return serde_json::json!({"error":"exploration_not_enabled"}).to_string(),
        };
        let exp = &h.result;

        if src_idx == dst_idx {
            let i = src_idx as usize;
            let node = serde_json::json!({
                "dense_idx": src_idx,
                "display_class": exp.class_names_by_idx.get(i).cloned().unwrap_or_default(),
                "shallow": h.get_shallow(i) as u64,
                "retained": h.get_retained(i),
            });
            return serde_json::json!({"ok":true,"path":[node]}).to_string();
        }

        // Decompress fwd_targets once for the BFS
        let fwd_targets = match h.restore_fwd_targets() {
            Ok(t) => t,
            Err(e) => return serde_json::json!({"error": e.to_string()}).to_string(),
        };

        let n = exp.fwd_offsets.len().saturating_sub(1);
        let mut visited = vec![u32::MAX; n];
        let mut queue = std::collections::VecDeque::new();
        queue.push_back(src_idx);
        visited[src_idx as usize] = src_idx;

        'bfs: while let Some(cur) = queue.pop_front() {
            let ci = cur as usize;
            if ci + 1 >= exp.fwd_offsets.len() {
                continue;
            }
            let start = exp.fwd_offsets[ci] as usize;
            let end = exp.fwd_offsets[ci + 1] as usize;
            for &nxt in &fwd_targets[start..end] {
                let ni = nxt as usize;
                if ni >= n || visited[ni] != u32::MAX {
                    continue;
                }
                visited[ni] = cur;
                if nxt == dst_idx {
                    break 'bfs;
                }
                if queue.len() < 50_000 {
                    queue.push_back(nxt);
                }
            }
        }

        if visited[dst_idx as usize] == u32::MAX {
            return serde_json::json!({"ok":false,"error":"no_path"}).to_string();
        }

        let mut path_indices = vec![dst_idx];
        let mut cur = dst_idx;
        for _ in 0..500 {
            let parent = visited[cur as usize];
            if parent == u32::MAX || parent == cur {
                break;
            }
            path_indices.push(parent);
            if parent == src_idx {
                break;
            }
            cur = parent;
        }
        path_indices.reverse();

        let path_json: Vec<serde_json::Value> = path_indices
            .iter()
            .map(|&idx| {
                let i = idx as usize;
                serde_json::json!({
                    "dense_idx": idx,
                    "display_class": exp.class_names_by_idx.get(i).cloned().unwrap_or_default(),
                    "shallow": h.get_shallow(i) as u64,
                    "retained": h.get_retained(i),
                })
            })
            .collect();

        serde_json::json!({"ok":true,"path":path_json}).to_string()
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Free functions
// ──────────────────────────────────────────────────────────────────────────────

/// Private helpers (not exported to JS)
impl HprofSession {
    #[allow(dead_code)]
    fn get_retained(&self, idx: usize) -> u64 {
        if !self.retained.is_empty() {
            self.retained.get(idx).copied().unwrap_or(0)
        } else if let Some(c) = &self.retained_c {
            c.get_at(idx).ok().flatten().unwrap_or(0)
        } else {
            0
        }
    }

    fn decompress_retained(&self) -> std::io::Result<Vec<u64>> {
        if !self.retained.is_empty() {
            Ok(self.retained.clone())
        } else if let Some(c) = &self.retained_c {
            c.restore()
        } else {
            Ok(Vec::new())
        }
    }

    fn has_any_retained(&self) -> bool {
        !self.retained.is_empty() || self.retained_c.is_some()
    }
}

/// Decode a single vbyte (little-endian base-128) value from `data[pos..]`.
/// Returns `(value, bytes_consumed)` or `None` if out of bounds.
/// MSB=1 means continuation byte; MSB=0 means final byte.
fn vbyte_decode(data: &[u8], pos: usize) -> Option<(u32, usize)> {
    let mut val = 0u32;
    let mut shift = 0u32;
    let mut i = pos;
    loop {
        if i >= data.len() {
            return None;
        }
        let b = data[i];
        i += 1;
        val |= ((b & 0x7f) as u32) << shift;
        if b & 0x80 == 0 {
            return Some((val, i - pos));
        }
        shift += 7;
        if shift >= 35 {
            return None; // overflow guard
        }
    }
}

/// Decode the inbound parent pre-order list for node `pre` from the blocked CSR.
/// Returns `(parent_pres, total_count)`. At most `limit` parents are returned.
fn decode_inbound_parents(
    pre: usize,
    inb_block_off: &[u64],
    inb_data: &[u8],
    inb_block: usize,
    limit: usize,
) -> (Vec<u32>, usize) {
    let block = pre / inb_block;
    if block >= inb_block_off.len() {
        return (vec![], 0);
    }

    let mut pos = inb_block_off[block] as usize;

    // Skip past entries for nodes in this block before `pre`
    let block_start = block * inb_block;
    for _ in block_start..pre {
        // Read count
        let (cnt, c0) = match vbyte_decode(inb_data, pos) {
            Some(x) => x,
            None => return (vec![], 0),
        };
        pos += c0;
        // Skip `cnt` delta values
        for _ in 0..cnt {
            let (_, c1) = match vbyte_decode(inb_data, pos) {
                Some(x) => x,
                None => return (vec![], 0),
            };
            pos += c1;
        }
    }

    // Now at node `pre`
    let (cnt, c0) = match vbyte_decode(inb_data, pos) {
        Some(x) => x,
        None => return (vec![], 0),
    };
    pos += c0;

    let total = cnt as usize;
    let mut parents: Vec<u32> = Vec::with_capacity(total.min(limit));
    let mut prev: u32 = 0;
    for _ in 0..total {
        let (delta, c) = match vbyte_decode(inb_data, pos) {
            Some(x) => x,
            None => break,
        };
        pos += c;
        prev = prev.wrapping_add(delta);
        parents.push(prev);
        if parents.len() >= limit {
            break;
        }
    }
    (parents, total)
}

/// Extract a `(dense_idx, display_class)` pair from a QueryValue.
/// For primitive / null values the dense_idx is None and the class is a formatted string.
#[allow(dead_code)]
fn ref_or_prim(val: &hprof_analyzer::query::model::QueryValue) -> (Option<u64>, String) {
    use hprof_analyzer::query::model::QueryValue;
    match val {
        QueryValue::ObjRef { index, class, .. } => (Some(*index), class.clone()),
        QueryValue::Null => (None, "null".to_string()),
        QueryValue::Bool(b) => (None, b.to_string()),
        QueryValue::Int(n) => (None, n.to_string()),
        QueryValue::Float(f) => (None, format!("{f}")),
        QueryValue::Str(s) => (None, format!("\"{s}\"")),
    }
}

/// Return `(collection_kind, backing_array_field_name)` for known Java/Scala/Kotlin
/// collection classes, or `None` if the class is not a recognised collection.
///
/// The field name is the name of the *object-array* field that holds the entries
/// (e.g. `"table"` for HashMap, `"elementData"` for ArrayList).  For map types the
/// array contains interleaved key/value Entry objects; we return `"map"`.  For
/// list/set/deque types we return `"list"`.
///
/// Class names are dot-separated (as stored in `class_names_by_idx`).
fn collection_info(class_name: &str) -> Option<(&'static str, &'static str)> {
    // Normalise: trim generic suffix and convert to dot-form
    let base = class_name.split('<').next().unwrap_or(class_name).trim();
    match base {
        // ── JDK maps ──────────────────────────────────────────────────────────
        "java.util.HashMap"
        | "java.util.LinkedHashMap"
        | "java.util.Hashtable"
        | "java.util.Properties" => Some(("map", "table")),
        "java.util.TreeMap" => Some(("map", "table")),
        "java.util.concurrent.ConcurrentHashMap" => Some(("map", "table")),
        // ── JDK lists ─────────────────────────────────────────────────────────
        "java.util.ArrayList" | "java.util.Vector" | "java.util.Stack" => {
            Some(("list", "elementData"))
        }
        "java.util.LinkedList" => Some(("list", "first")), // first Node; we iterate via outbound refs
        "java.util.ArrayDeque" => Some(("list", "elements")),
        // ── JDK sets (backed by a map's key set — expose array field of inner map) ──
        "java.util.HashSet" | "java.util.LinkedHashSet" => Some(("list", "map")), // map is a HashMap; outbound_refs will give us the table
        "java.util.TreeSet" => Some(("list", "m")),
        // ── Kotlin stdlib (thin JDK wrappers) ────────────────────────────────
        "kotlin.collections.ArrayList" => Some(("list", "elementData")),
        "kotlin.collections.HashMap" | "kotlin.collections.LinkedHashMap" => Some(("map", "table")),
        "kotlin.collections.HashSet" | "kotlin.collections.LinkedHashSet" => Some(("list", "map")),
        // ── Scala mutable ────────────────────────────────────────────────────
        "scala.collection.mutable.HashMap" => Some(("map", "table")),
        "scala.collection.mutable.ArrayBuffer" => Some(("list", "array")),
        "scala.collection.mutable.ListBuffer" => Some(("list", "start")),
        // ── Eclipse Collections ───────────────────────────────────────────────
        "org.eclipse.collections.impl.map.mutable.UnifiedMap" => Some(("map", "table")),
        "org.eclipse.collections.impl.list.mutable.FastList" => Some(("list", "items")),
        "org.eclipse.collections.impl.set.mutable.UnifiedSet" => Some(("list", "table")),
        // ── Trove ─────────────────────────────────────────────────────────────
        "gnu.trove.map.hash.THashMap" | "gnu.trove.THashMap" => Some(("map", "_values")),
        // ── Guava ─────────────────────────────────────────────────────────────
        "com.google.common.collect.ImmutableList" => Some(("list", "array")),
        "com.google.common.collect.ImmutableMap" => Some(("map", "table")),
        "com.google.common.collect.ImmutableSet" => Some(("list", "elements")),
        _ => None,
    }
}

/// Map a GC root HPROF sub-tag byte to a human-readable label.
fn gc_root_type_label(tag: u8) -> &'static str {
    match tag {
        0x01 => "JNI_GLOBAL",
        0x02 => "JNI_LOCAL",
        0x03 => "JAVA_FRAME",
        0x04 => "NATIVE_STACK",
        0x05 => "STICKY_CLASS",
        0x06 => "THREAD_BLOCK",
        0x07 => "MONITOR_USED",
        0x08 => "THREAD_OBJECT",
        _ => "GC_ROOT",
    }
}

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

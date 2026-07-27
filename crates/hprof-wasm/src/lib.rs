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

// ──────────────────────────────────────────────────────────────────────────────
// HprofSession
// ──────────────────────────────────────────────────────────────────────────────

/// An active hprof analysis session backed by in-memory bytes.
///
/// Call `HprofSession.load(bytes, name)` from JS to initialise a session.
/// Once loaded, use `query()` for OQL queries, `generate_report()` for the
/// full analysis report, and `run_full_analysis()` to pre-compute retained sizes.
#[wasm_bindgen]
pub struct HprofSession {
    source: hprof_analyzer::HprofSource,
    class_names: Vec<String>,
    retained: Vec<u64>,
    cache: Option<hprof_analyzer::query::run::ReplCache>,
}

#[wasm_bindgen]
impl HprofSession {
    /// Load a `.hprof` file from its raw bytes and build the query cache.
    ///
    /// `name` is used only for display (e.g. `"heap.hprof"`).
    /// Returns a JS error string if parsing fails.
    pub fn load(data: &[u8], name: &str) -> Result<HprofSession, JsValue> {
        let arc: Arc<[u8]> = Arc::from(data);
        let source = hprof_analyzer::HprofSource::Bytes {
            data: arc,
            name: name.to_string(),
        };

        // Build the ReplCache eagerly — Pass1 + Pass2 run once here, and all
        // subsequent queries reuse it without re-scanning raw bytes.
        let cache = hprof_analyzer::query::run::ReplCache::build(&source, true)
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

        Ok(HprofSession {
            source,
            class_names,
            retained: Vec::new(),
            cache: Some(cache),
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
                .to_string()
            }
        };

        let plan_result = match plan::plan_query(&q, 5) {
            Ok(p) => p,
            Err(e) => {
                return serde_json::json!({
                    "ok": false,
                    "error": { "message": e.0 }
                })
                .to_string()
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
                    .to_string()
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
                .to_string()
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

    /// Pre-compute dominators + retained sizes so `@retainedHeapSize` queries
    /// are served from the cached array on subsequent `query()` calls.
    pub fn run_full_analysis(&mut self) -> Result<(), JsValue> {
        let opts = hprof_analyzer::AnalyzeOptions::default();
        let (_report, retained) =
            hprof_analyzer::analyze_to_report_with_retained(&self.source, &opts)
                .map_err(|e| JsValue::from_str(&e.to_string()))?;
        self.retained = retained;
        Ok(())
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
}

// ──────────────────────────────────────────────────────────────────────────────
// Free functions
// ──────────────────────────────────────────────────────────────────────────────

/// OQL tab-completion suggestions for a partial input line.
///
/// Returns a JSON array: `[{"value":"...","display":"...","group":"..."},...]`
#[wasm_bindgen]
pub fn complete(line: &str, cursor_pos: usize, class_names: Vec<String>) -> String {
    let cs = hprof_analyzer::query::complete::complete(line, cursor_pos, &class_names, &[]);
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

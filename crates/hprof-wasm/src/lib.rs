//! hprof-wasm: WebAssembly bindings for the hprof-analyzer library.
//!
//! Exposes a `HprofSession` JS class that accepts raw `.hprof` bytes,
//! runs OQL queries, and returns JSON results.
//!
//! # File I/O and targets
//! `wasm32-unknown-unknown` has no filesystem. The `load()` method writes
//! bytes to `/tmp/` which only works on native or WASI targets.
//! Task 7's build script handles the actual deployment strategy.

use wasm_bindgen::prelude::*;

#[wasm_bindgen(start)]
pub fn init() {
    console_error_panic_hook::set_once();
}

// ──────────────────────────────────────────────────────────────────────────────
// HprofSession
// ──────────────────────────────────────────────────────────────────────────────

/// An active hprof analysis session.
///
/// Call `HprofSession.load(bytes)` from JS to initialize a session from raw
/// `.hprof` bytes. Once loaded, use `query()` for OQL queries and
/// `run_full_analysis()` for dominator/retained-size data.
#[wasm_bindgen]
pub struct HprofSession {
    /// Path written by `load()` – only valid when running with filesystem access.
    session_path: String,
    /// Class names extracted during load (for completion).
    class_names: Vec<String>,
    /// Per-object retained sizes, populated by `run_full_analysis()`.
    retained: Vec<u64>,
}

#[wasm_bindgen]
impl HprofSession {
    /// Load a `.hprof` file from its raw bytes.
    ///
    /// Writes the bytes to `/tmp/hprof_wasm_session.hprof` and runs Pass1
    /// to build the class-name index used for OQL completion.
    ///
    /// # Errors
    /// Returns a JS error string if the write or parse fails.
    ///
    /// # WASM note
    /// `wasm32-unknown-unknown` has no filesystem. This will return an error
    /// in a bare browser environment. Task 7's build script configures the
    /// correct WASM target (wasmer/wasm-pack bundler) with filesystem support.
    pub fn load(data: &[u8]) -> Result<HprofSession, JsValue> {
        #[cfg(target_arch = "wasm32")]
        {
            // wasm32-unknown-unknown has no std::fs. Return a descriptive error
            // so the caller knows they need the Task 7 build (WASI/MEMFS target).
            let _ = data;
            return Err(JsValue::from_str(
                "file I/O unavailable in this wasm32-unknown-unknown build; \
                 use Task 7's WASI-enabled build for actual file loading",
            ));
        }

        #[cfg(not(target_arch = "wasm32"))]
        {
            let path = "/tmp/hprof_wasm_session.hprof";

            std::fs::write(path, data)
                .map_err(|e| JsValue::from_str(&format!("fs::write failed: {e}")))?;

            let p1 = hprof_analyzer::Pass1::run(path, false)
                .map_err(|e| JsValue::from_str(&e.to_string()))?;

            let class_names: Vec<String> = p1
                .class_map
                .values()
                .filter_map(|ci| p1.strings.get(&ci.name_id).map(|s| s.replace('/', ".")))
                .collect();

            Ok(HprofSession {
                session_path: path.to_string(),
                class_names,
                retained: Vec::new(),
            })
        }
    }

    /// Run an OQL query and return a JSON string.
    ///
    /// Success format:
    /// `{"ok":true,"result":{"columns":[...],"rows":[...],"row_count":N}}`
    ///
    /// Error format:
    /// `{"ok":false,"error":{"message":"..."}}`
    pub fn query(&self, oql: &str) -> String {
        #[cfg(target_arch = "wasm32")]
        {
            let _ = oql;
            return serde_json::json!({
                "ok": false,
                "error": { "message": "query unavailable in wasm32-unknown-unknown build" }
            })
            .to_string();
        }

        #[cfg(not(target_arch = "wasm32"))]
        self.run_query_native(oql)
    }

    /// Returns `true` if `run_full_analysis()` has been called and retained
    /// sizes are available.
    pub fn has_retained(&self) -> bool {
        !self.retained.is_empty()
    }

    /// Run dominator + retained-size analysis.
    ///
    /// After this call `has_retained()` returns `true` and queries using
    /// `@retainedHeapSize` are served from the cached array without re-running
    /// the full pipeline.
    pub fn run_full_analysis(&mut self) -> Result<(), JsValue> {
        #[cfg(target_arch = "wasm32")]
        {
            return Err(JsValue::from_str(
                "run_full_analysis unavailable in wasm32-unknown-unknown build",
            ));
        }

        #[cfg(not(target_arch = "wasm32"))]
        {
            let opts = hprof_analyzer::AnalyzeOptions::default();
            let (_report, retained) =
                hprof_analyzer::analyze_to_report_with_retained(&self.session_path, &opts)
                    .map_err(|e| JsValue::from_str(&e.to_string()))?;
            self.retained = retained;
            Ok(())
        }
    }

    /// Generate the analysis report as a JSON string.
    ///
    /// This runs the full analysis pipeline (dominators + retained sizes).
    /// The returned JSON matches the `Report` schema.
    pub fn generate_report(&mut self) -> Result<String, JsValue> {
        #[cfg(target_arch = "wasm32")]
        {
            return Err(JsValue::from_str(
                "generate_report unavailable in wasm32-unknown-unknown build",
            ));
        }

        #[cfg(not(target_arch = "wasm32"))]
        {
            let opts = hprof_analyzer::AnalyzeOptions::default();
            let (report, retained) =
                hprof_analyzer::analyze_to_report_with_retained(&self.session_path, &opts)
                    .map_err(|e| JsValue::from_str(&e.to_string()))?;
            self.retained = retained;
            serde_json::to_string(&report)
                .map_err(|e| JsValue::from_str(&e.to_string()))
        }
    }

    /// Returns a JSON array of all built-in named queries.
    ///
    /// Each entry: `{"name":"...","display":"...","group":"...","needs_retained":bool}`
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

    /// Returns a JSON array of class names loaded from the session.
    /// Useful for populating completion lists in the browser UI.
    pub fn class_names(&self) -> String {
        serde_json::to_string(&self.class_names).unwrap_or_else(|_| "[]".to_string())
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Native-only helpers (not exported to WASM)
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(not(target_arch = "wasm32"))]
impl HprofSession {
    /// Execute an OQL query using the resident-only (Pass1-only) fast path or the
    /// retained-size path if `run_full_analysis()` was previously called.
    fn run_query_native(&self, oql: &str) -> String {
        use hprof_analyzer::query::{optimize, parse, plan, run};

        // Build a temporary ReplCache for this query.
        let cache = match run::ReplCache::build(&self.session_path, true) {
            Ok(c) => c,
            Err(e) => {
                return serde_json::json!({
                    "ok": false,
                    "error": { "message": e.to_string() }
                })
                .to_string()
            }
        };

        // Parse.
        let q = match parse::parse(oql) {
            Ok(q) => q,
            Err(e) => {
                return serde_json::json!({
                    "ok": false,
                    "error": { "message": e.0 }
                })
                .to_string()
            }
        };

        // Plan.
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

        // Optimize.
        let optimized = optimize::optimize(plan_result, &q, &optimize::SchemaStats::default());

        // Execute.
        let pairs = vec![(q, optimized)];
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
        // Check if query itself produced an error result.
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
}

// ──────────────────────────────────────────────────────────────────────────────
// Free functions
// ──────────────────────────────────────────────────────────────────────────────

/// OQL tab-completion suggestions for a partial input line.
///
/// Returns a JSON array:
/// `[{"value":"...","display":"...","group":"..."},...]`
///
/// `class_names` and `field_names` should come from a loaded `HprofSession`;
/// pass empty arrays when no session is active.
#[wasm_bindgen]
pub fn complete(line: &str, cursor_pos: usize, class_names: Vec<String>) -> String {
    let cs =
        hprof_analyzer::query::complete::complete(line, cursor_pos, &class_names, &[]);
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

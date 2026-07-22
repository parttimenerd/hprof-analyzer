# Query Visualization Directives + Sorting/Percentiles Design

**Date:** 2026-07-22

**Goal:** Let users declare a visualization (table / histogram / piechart / treemap) per OQL query via an in-query `-- @viz` directive, rendered in HTML (rich charts) and Markdown (ASCII bars / table+note). The directive travels with the query text through all three entry points: `--query`/`--query-file`, config file, and the interactive shell. Along the way, close a latent ORDER BY sorting gap and add `PERCENTILE`/`MEDIAN` aggregates.

**Architecture:** The `-- @viz` directive is stripped from the query text *before* the OQL parser sees it (the lexer has no `--` comment rule), by a new `src/query/viz.rs` module. The parsed `VizSpec` rides on `QueryResult` and is consumed by the md/html renderers. OQL parse/plan/execute are otherwise untouched, except for two orthogonal engine additions (general ORDER BY sort; percentile aggregates).

**Tech Stack:** Rust (logos+chumsky OQL engine, binary-only crate `hprof-analyzer`); TypeScript/React + d3-hierarchy + chart.js for the self-contained HTML report (`web/`).

---

## Scope decisions (user-confirmed)

- **Declaration mechanism:** in-query comment directive `-- @viz <kind> [named args]`. Travels with the query text, so all three surfaces get it for free.
- **Viz kinds:** `table` (default no-op), `histogram`, `piechart`, `treemap`.
- **Output formats:** HTML (rich charts) + Markdown (ASCII bars for histogram/piechart, table+note for treemap). JSON serializes the `viz` metadata for free (no drawing).
- **Column mapping:** named args with positional fallback (`label=`, `value=`).
- **Malformed directive / unchartable data:** warn + fall back to table (set `note`, still return data). Never hard-fail the query.
- **Caps:** no default cap (users use `LIMIT`); optional `cap=<n>` in the directive limits *chart* rows only (top-N by value), table shows all.
- **Sorting:** fix the latent general ORDER BY sort as its **own bug-fix commit first**, then build viz on top.
- **Percentiles:** `PERCENTILE(@attr, <p>)` and `MEDIAN(@attr)` as new scalar aggregate functions. GROUP BY / per-group percentiles are **out of scope** (no GROUP BY exists today).

## Out of scope

- `GROUP BY` and per-group aggregation/percentiles.
- Chart interactivity beyond what the existing `charts.tsx` components already provide.
- Any new default row caps on chart rendering.
- JSON-side chart *drawing* (JSON only carries the declaration).

---

## Standing constraints (MUST hold)

- NEVER `git push` / `gh pr create`.
- Binary-only crate: unit tests `cargo test --bin hprof-analyzer <ONE filter>` (one filter per call); NEVER `--lib`. Integration: `--test cli_query` / `--test cli_unified`. Debug builds while iterating.
- Commit ONLY specific named files (never `git add -A`/`.`). New commits only, never `--amend`, never `--no-verify`.
- Do NOT run `cargo fmt` / `clippy --fix` / workspace formatters.
- Exceed the plan's test list; write actionable error/warning messages.
- MEMORY-CRITICAL: a run with NO viz/percentile query must be byte-for-byte and RSS-identical to today. The percentile accumulator (a per-arg `Vec<f64>`) is armed ONLY when a query uses PERCENTILE/MEDIAN; do NOT introduce any large flat per-object `Vec`.
- Subagent dispatches use model sonnet.

---

## Component 1: `src/query/viz.rs` (new module) — directive parsing

### `VizKind`
```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum VizKind {
    Table,
    Histogram,
    Piechart,
    Treemap,
}
```

### `VizSpec` (attached to `QueryResult`)
```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct VizSpec {
    pub kind: VizKind,
    /// Column name (alias or derived) for the label axis; None => positional fallback.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label_col: Option<String>,
    /// Column name for the numeric value axis; None => positional fallback.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value_col: Option<String>,
    /// Optional top-N cap for the CHART ONLY (table always shows all rows).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cap: Option<usize>,
}
```

### `split_directive`
```rust
/// Extract a leading `-- @viz ...` directive from the query text.
/// Returns (cleaned_oql, Option<VizSpec>, Option<warning>). Only the FIRST
/// `-- @viz` line is consumed and removed; any other `--` line is left
/// untouched (the OQL parser will reject it, preserving today's behavior — we
/// are NOT adding general comment support). A malformed directive still removes
/// its line and returns (cleaned, None, Some(reason)).
pub fn split_directive(text: &str) -> (String, Option<VizSpec>, Option<String>);
```

Directive grammar (single line, case-insensitive keyword):
```
-- @viz <kind> [label=<col>] [value=<col>] [cap=<n>]
```
- `<kind>` ∈ `table | histogram | piechart | treemap`.
- `label=` / `value=` take a column name = a SELECT alias (`AS foo` → `foo`) or a derived name (`@retainedHeapSize`, `COUNT(*)`). A leading `@` in the arg value is tolerated and stripped so `value=@retainedHeapSize` == `value=retainedHeapSize`.
- `cap=<n>` is a positive integer; `cap=0` or non-integer → treated as malformed (warn + fall back, cap ignored).
- Unknown `<kind>` / any malformed directive → `split_directive` still removes the directive line (so the OQL parses) and returns `(cleaned_oql, None, Some(warning))`. The signature is:
  ```rust
  pub fn split_directive(text: &str) -> (String, Option<VizSpec>, Option<String>);
  //                                     cleaned_oql, spec,        warning
  ```
  On a well-formed directive: `(cleaned, Some(spec), None)`. On no directive: `(text, None, None)`. On malformed: `(cleaned, None, Some(reason))`. The intake site turns a `Some(warning)` into `result.note`. Observable contract: malformed directive ⇒ query runs, renders as table, `note` explains why. Never a hard error from the directive itself.

### `resolve_columns`
```rust
/// Map a VizSpec to (label_idx, value_idx) against the result columns/rows.
/// Named args looked up by column name; else positional fallback:
///   value_idx = first numeric column (Int/Float across a sample of rows)
///   label_idx = first non-value column (usually col 0)
/// Returns Err(reason) if the chart cannot be built (unknown column name,
/// no numeric column for value, fewer than the required columns). The caller
/// converts Err into a table fallback + note (never a hard query error).
pub fn resolve_columns(
    spec: &VizSpec,
    columns: &[QueryColumn],
    rows: &[Vec<QueryValue>],
) -> Result<(usize, usize), String>;
```

- A column is "numeric" if every non-Null cell in it is `Int` or `Float`.
- `table` kind needs no columns (no-op).
- `histogram`/`piechart`/`treemap` require ≥1 label + 1 numeric value column.

---

## Component 2: `QueryResult.viz` field + intake wiring

### model.rs
Add to `QueryResult`:
```rust
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub viz: Option<VizSpec>,
```
All existing `QueryResult { .. }` literals gain `viz: None` (execute.rs finish x2, stage_runner late-phase constructors, error results, tests). This is mechanical.

### Intake choke point
Wherever raw query text is turned into a `(Query, QueryPlan)` for execution, call `split_directive` first: parse the cleaned OQL, and stash the `VizSpec` so it can be attached to the resulting `QueryResult`. Entry points:
- `--query` / `--query-file` (analyze subcommand, main.rs).
- Config-file query entries (see Component 5).
- Interactive shell `run_one` (repl.rs).

**Attachment:** the `VizSpec` is known at intake but the `QueryResult` is produced deep in the executor. Rather than thread `VizSpec` through every executor signature, attach it AFTER the result returns, at the same intake site: `result.viz = spec;` (and if `resolve_columns` fails, `result.note = Some(warning); result.viz = None`). This keeps the executor ignorant of viz. The resolve check runs at attach time (columns+rows are known then).

---

## Component 3: ORDER BY general sort (independent bug-fix, lands FIRST)

**Bug:** ORDER BY is parsed and validated, but rows are only actually sorted on the late/retained path when `ob.key == @retainedHeapSize` (stage_runner.rs:877, plan.rs:708). A plain `SELECT @displayName, @usedHeapSize FROM C ORDER BY @usedHeapSize DESC` parses, validates, and returns rows in **scan order** — silently unsorted.

**Fix:** in `SingleScanExecutor::finish` (execute.rs:829), when `self.query.order_by` is `Some(ob)` AND the query is NOT the retained-late path (i.e. not carry mode; the late path already sorts), sort `self.rows` before building the result:
- Find the column index whose derived/alias name matches `ob.key` (reuse `query_columns` name derivation). If the ORDER BY key is not a projected column, evaluate it — but for v1, require the key to be a projected column OR a known scan-time attr already in the row; if the key is not resolvable to a row column, keep scan order and set a `note` (do not hard-fail).
- Sort with existing `compare_query_values` (execute.rs:1226), reversed for DESC.
- Apply BEFORE LIMIT (LIMIT is applied at scan time today for the non-carry path — see the ordering note below).

**LIMIT-before-sort hazard:** today the non-carry scan applies LIMIT during `visit_instance` (execute.rs:~955), i.e. it takes the first N in scan order, THEN finish() would sort only those N. That yields the wrong top-N for an ORDER BY. **Fix:** when `order_by.is_some()` on the non-carry path, do NOT apply the scan-time LIMIT (collect all matches), sort in finish(), then truncate to LIMIT. Guard this so no-ORDER-BY queries keep the existing early-stop LIMIT (byte-identical). Document the memory trade-off: an ORDER BY query now buffers all matched rows before truncating — acceptable and query-gated.

**Tests:** `SELECT @a, @b FROM C ORDER BY @b DESC LIMIT 5` returns the 5 largest `@b` (not the first 5 scanned); ASC path; ties stable; ORDER BY a non-projected attr (documented behavior); no-ORDER-BY query byte-identical.

This is committed on its own before any viz code.

---

## Component 4: `PERCENTILE` / `MEDIAN` aggregates

### ast.rs
```rust
pub enum AggFunc {
    Count, Sum, Min, Max, Avg,
    Percentile(u8), // p in 1..=100
    Median,         // == Percentile(50)
}
```
`MEDIAN(@x)` parses to `AggFunc::Median`; `PERCENTILE(@x, 95)` to `AggFunc::Percentile(95)`.

### parse.rs
- `PERCENTILE` takes two args: an attr/expr and an integer percentile literal `1..=100`. Out-of-range → actionable parse error.
- `MEDIAN` takes one arg.
- Add `PERCENTILE`, `MEDIAN` to `RESERVED`.

### execute.rs accumulator
`AggAcc` gains a variant holding a `Vec<f64>` (collect all numeric arg values for the matched set). At finalize: sort, pick the value at the p-th percentile (nearest-rank method: `idx = ceil(p/100 * n) - 1`, clamped), return as Float (or Int if all inputs were integral — keep as Float for simplicity). Median = Percentile(50).

**Memory:** the `Vec<f64>` is armed ONLY when the query's SELECT contains a Percentile/Median aggregate (`init_agg_acc` returns the collecting variant only for those; every other query keeps `AggAcc::None`/scalar accumulators). No change to non-percentile runs.

**Cross-phase guard:** percentile over a toString-filtered / retained-late set follows the same rule as SUM/AVG today — rejected at plan time with an actionable message (only COUNT is foldable late). Percentile over a plain scan-time attr works.

**Tests:** `SELECT PERCENTILE(@usedHeapSize, 95) FROM C` returns a plausible p95 ≥ median; `MEDIAN(@x)` == `PERCENTILE(@x, 50)`; p=100 == MAX, p=1 near MIN; empty set → Null; `PERCENTILE(@x, 0)`/`101` → parse error; percentile + retained-WHERE → plan error.

---

## Component 5: config-file query entries

`src/collection_config.rs` `ConfigFile` gains an optional `[[query]]` array:
```toml
[[query]]
name = "big objects"
oql = """
-- @viz treemap value=@retainedHeapSize label=@displayName
SELECT @displayName, @retainedHeapSize FROM java.lang.Thread
"""
```
```rust
#[derive(serde::Deserialize)]
struct RawQuery { name: Option<String>, oql: String }

#[derive(serde::Deserialize)]
struct ConfigFile {
    #[serde(default)] collection: Vec<RawEntry>,
    #[serde(default)] query: Vec<RawQuery>,   // NEW
}
```
A loader returns `Vec<(name, oql_text)>`; the analyze path feeds these through the same intake choke point as `--query` (directive split included, since it's in the `oql` text). Config-file queries and `--query` queries concatenate (config first, then CLI), each rendered under "## Custom Queries".

**Tests:** a config with a `[[query]]` block produces a report query; malformed oql → error result (not a crash); no `[[query]]` block → today's behavior unchanged (byte-identical).

---

## Component 6: rendering

### Markdown — `render_md.rs::render_custom_queries`
For each query, switch on `result.viz`:
- `None` or `Table` → today's table (unchanged).
- `Histogram` → unicode bar chart: for each row (optionally top `cap` by value), `label  ████████ <value>` where bar length ∝ value / max_value; a scale line. If `resolve_columns` failed, table + the `note`.
- `Piechart` → `label  ██████ <pct>%` where pct = value / sum. Top `cap` rows; remaining bucketed into "(other) …%" when capped.
- `Treemap` → today's table + `> Shown as a treemap in the HTML report.`
- A small shared helper `ascii_bar(frac: f64, width: usize) -> String` (unicode full/partial blocks).

Charted sub-sections still print the fenced OQL and (for histogram/piechart) MAY also print the underlying table below the bars for exact values — **DECISION:** print bars, then the full table underneath (bars are the summary, table is the detail). Treemap prints only the table (+note).

### HTML — `web/src/App.tsx` + `charts.tsx` + `types.ts`
- `types.ts`: extend the `QueryResult` TS type with `viz?: { kind, label_col?, value_col?, cap? }`.
- New `QueryViz` component in `App.tsx` (or `charts.tsx`): given a `QueryResult` with `viz`, resolve label/value column indices (mirror the Rust `resolve_columns` fallback), map rows to `{label, value}` pairs, apply `cap` (top-N by value), and render:
  - `histogram` → existing `Bar` (`charts.tsx:148`).
  - `piechart` → existing `Pie` (`charts.tsx:~51`).
  - `treemap` → existing d3 `treemap` (`charts.tsx:444`), fed the pairs instead of the package tree (extract the layout into a reusable `<Treemap data={{label,value}[]}/>` if the current one is hard-wired to packages).
  - fallback/`table` → the existing query table.
- Wrap charts in the existing `ChartOrNote` so "too few rows to chart" degrades to a note.
- The existing "Custom Queries" section in the HTML report renders each query with its chart (or table).

**Rebuild the bundle:** `web/dist/bundle.js` is committed; rebuild it (`npm run build` in `web/`) and commit the updated bundle. The self-contained HTML test (`tests/html_selfcontained.rs`) must still pass.

---

## Data flow (end-to-end)

```
query text (any of: --query, config [[query]], shell)
  → viz::split_directive  → (cleaned_oql, Option<VizSpec>, Option<String> warning)
  → parse(cleaned_oql) → plan → execute → QueryResult{columns,rows,...}
  → at intake site:
        if let Some(w) = warning { result.note = Some(w); }        // malformed directive
        else if let Some(spec) = viz_spec {
            match viz::resolve_columns(&spec, cols, rows) {
                Ok(_)   => result.viz = Some(spec),
                Err(w)  => result.note = Some(w),                  // unchartable data
            }
        }
  → report.queries.push(result)
  → render_md / web (App.tsx) read result.viz, draw chart or table
```

## Error / fallback matrix

| Situation | Behavior |
|-----------|----------|
| Unknown `<kind>` | run query, table, note "ignored @viz: unknown kind" |
| `value=` column missing/unknown | run query, table, note |
| value column non-numeric | run query, table, note |
| `< required` columns for kind | run query, table, note |
| `cap=0`/non-int | ignore cap, chart with no cap |
| valid directive | chart (HTML) / bars or table+note (MD) |
| OQL itself invalid | today's error result (unchanged) |
| ORDER BY key not a projected column | keep scan order + note (v1) |
| PERCENTILE p out of 1..=100 | parse error (hard) |
| PERCENTILE over retained/toString-late set | plan error (hard) |

## Testing summary

- `viz.rs` unit: directive parse (all kinds, named/positional, cap, `@`-strip, unknown kind, malformed); `resolve_columns` (named hit, positional fallback, non-numeric value, missing column, <2 cols).
- ORDER BY sort: top-N correctness, ASC/DESC, ties, non-projected key, no-ORDER-BY byte-identical.
- Percentile: p95/median/p100==max/p1/empty/out-of-range/late-set-rejected.
- Config `[[query]]`: produces report query; no block => byte-identical.
- Integration (`cli_query`/`cli_unified`): `--query '-- @viz histogram ...'` runs and the md report contains bars; a plain query is byte-identical to today.
- HTML: `html_selfcontained` passes with rebuilt bundle; a charted query renders a chart element.
- MEMORY: a no-viz, no-percentile run is byte/RSS-identical (verify via `cli_unified`).

## Critical files

- Create: `src/query/viz.rs`
- `src/query/model.rs` — `QueryResult.viz`, re-export `VizSpec`/`VizKind`
- `src/query/ast.rs` — `AggFunc::Percentile`/`Median`
- `src/query/parse.rs` — PERCENTILE/MEDIAN grammar + RESERVED
- `src/query/execute.rs` — `finish()` ORDER BY sort + LIMIT-defer; percentile accumulator (`AggAcc`, `init/fold/finalize_agg_acc`)
- `src/query/plan.rs` — percentile cross-phase guard; ORDER BY plan flags
- `src/main.rs` — intake choke point: split_directive + attach viz for `--query`/`--query-file`/config queries
- `src/query/repl.rs` — shell intake: split_directive + ASCII-bar print
- `src/collection_config.rs` — `[[query]]` entries
- `src/report/render_md.rs` — `render_custom_queries` viz switch + `ascii_bar`
- `web/src/types.ts`, `web/src/App.tsx`, `web/src/charts.tsx` — `QueryViz` component
- `web/dist/bundle.js` — rebuilt
- `schema/report.schema.json` — regenerated (viz field)
- Tests: `tests/cli_query.rs`, `tests/cli_unified.rs`, `tests/html_selfcontained.rs`, unit tests in the touched modules

## Verification

1. `cargo test --bin hprof-analyzer` (broad), `--test cli_query`, `--test cli_unified`, `--test html_selfcontained` — all PASS.
2. Live on `tests/fixtures/dump_4_philosophers.hprof`: `--query '-- @viz piechart value=n\nSELECT @clazz AS c, COUNT(*) AS n FROM ...'` renders bars in md; HTML shows a pie.
3. `SELECT @a,@b FROM C ORDER BY @b DESC LIMIT 5` returns the true top-5 by `@b`.
4. `SELECT PERCENTILE(@usedHeapSize,95) FROM C` returns a sane p95.
5. No-viz/no-percentile run byte/RSS-identical to today.
6. `cargo build --release` succeeds; binary size ~unchanged (no new Rust deps).
7. Do NOT push or open a PR.

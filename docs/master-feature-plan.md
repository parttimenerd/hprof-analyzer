# hprof-analyzer: Master Feature Plan

> Compiled 2026-07-29.  
> Evidence base: Eclipse MAT source code, JProfiler/YourKit/VisualVM/JMC/JOverflow docs and source,
> IBM HeapAnalyzer docs, async-profiler, Heaptrack, Go pprof, .NET dotMemory, GCeasy, 11 academic
> papers (LeakBot, Cork, Yeti, HeapViz, Maxwell graph mining, AntTracks, Xu container profiling,
> CSUR 2022 survey), HackerNews verbatim quotes, Reddit r/java community pain points, 10 production
> OOM case studies.
>
> Full research document: `docs/heap-analysis-research.md`

---

## Executive Summary

Research across five commercial tools, eleven academic papers, two community platforms, and ten
production case studies converges on the same two questions:

1. **"Who is holding this object alive?"** — every heap dump investigation ends here
2. **"Which objects should have been collected?"** — thread-local leaks, classloader leaks, caches
   without eviction are the top-3 production causes

The features below are grouped by implementation tier:
- **Tier A**: No new backend passes — all data already in the report JSON or in memory at build time
- **Tier B**: One additional field-decode scan per flag
- **Tier C**: New architectural piece (multi-dump support)

---

## Part 1 — Object Graph & Dominator Navigation

> **Why these first**: @kohlerm (HN): "Supporting a dominator tree view is IMHO a crucial feature
> you will need sooner or later." StackOverflow #1 voted heap question (2,847 votes): "who holds
> this reference?" These are the most-demanded features across every source.

### 1.1 Outbound Reference Graph Click-Through ★★★ (Tier A) — Status: PARTIAL

**What**: Expandable outbound-reference tree for the top retained objects in "Biggest Objects".
Equivalent to Eclipse MAT's "Outgoing References" tree (`ObjectListResult.Outbound`).

**Why now**: The #1 missing feature in the HTML report. Users see `HashMap (1.2 GB)` but cannot
drill into its fields without switching to a separate tool.

**Architecture** (key constraint already solved):
`g.fwd_offsets`/`g.fwd_targets` are consumed at `main.rs:941` during inbound CSR construction —
BEFORE dominators are computed and BEFORE `build_model` is called. The solution is to capture
the outbound edges for top-2000 objects by shallow heap (a proxy for "will be in biggest objects")
BEFORE the CSR is consumed.

**Implementation steps** (see also the existing plan at `docs/` or `.claude/plans/`):

1. **`src/pass2/model.rs`** — ✅ DONE:
   - `ObjGraphCapture` struct (`HashMap<u32, Vec<(u32, u16)>>` + `field_name_pool` + `captured` set)
   - `capture_obj_graph_edges(g, top_n, edge_cap)` function
   - `obj_graph_edges: Option<ObjGraphCapture>` field on `Graph`

2. **`src/main.rs`** — ❌ TODO:
   - Non-MAT path (before line 941): call `capture_obj_graph_edges` before `build_from_fwd`
   - MAT path (before line 2258): call before `drop(fwd_targets)`
   - Constants: `GRAPH_CAPTURE_TOP_N = 2000`, `GRAPH_EDGE_CAP = 50`
   ```rust
   g.obj_graph_edges = Some(crate::pass2::capture_obj_graph_edges(
       &g, GRAPH_CAPTURE_TOP_N, GRAPH_EDGE_CAP));
   ```

3. **`src/report/model.rs`** — ❌ TODO:
   ```rust
   pub struct ObjGraphEdge {
       pub field_name: String,
       pub child: Box<ObjGraphNode>,
   }
   pub struct ObjGraphNode {
       pub obj_index_1based: usize,
       pub display_class: String,
       pub shallow: u64,
       pub retained: u64,
       pub edges: Vec<ObjGraphEdge>,
       pub edges_truncated: bool,   // edge_cap hit
       pub edges_unknown: bool,     // not in capture set
   }
   // Add to Report:
   pub obj_graph: Vec<ObjGraphNode>,  // #[serde(default, skip_serializing_if="Vec::is_empty")]
   ```
   Also bump `SCHEMA_VERSION` 9 → 10.

4. **`src/report/build.rs`** — ❌ TODO:
   - `build_obj_graph(g, biggest_objects, max_depth=3, max_children=30) -> Vec<ObjGraphNode>`
   - Recursive `build_node(g, cap, idx, depth, max_children, visited)` with cycle guard via `HashSet`
   - Called from `build_model()` after `build_top_consumers()`

5. **`web/src/App.tsx`** — ❌ TODO:
   - `ObjectGraphTree` React component: collapsible tree with `▶`/`▼` toggles
   - Each row: `field_name → ClassName @ shallow / retained`
   - `edges_truncated` → "… more edges not shown"
   - `edges_unknown` → "▸ (deeper nodes not captured)"
   - Wire into Biggest Objects table: rows with `obj_graph[i]` get an expand toggle

6. **`web/src/styles.css`** — ❌ TODO:
   ```css
   .obj-graph-tree { font-size: 0.82rem; font-family: monospace; margin: 0.4rem 0 0.6rem 1rem; }
   .obj-graph-row  { display: flex; align-items: baseline; gap: 0.5rem; padding: 1px 0; }
   .obj-graph-toggle { width: 1rem; text-align: center; color: var(--muted); flex-shrink: 0; }
   .obj-graph-field  { color: var(--muted); min-width: 8ch; }
   .obj-graph-class  { flex: 1; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
   .obj-graph-size   { color: var(--muted); white-space: nowrap; }
   ```

**Memory budget**: 2000 × 50 × 8 bytes ≈ 800 KB capture; freed after `build_model`. Report JSON:
~3.2 MB uncompressed for 20 roots × depth-3 trees; ~300 KB deflated in HTML.

**Feature flag**: None (always-on). Field names only visible when `--ref-paths` was used.

**Reference**: MAT `ObjectListResult.java`, `IObject.getOutboundReferences()`, `NamedReference`

---

### 1.2 Dominator Tree Explorer for Top Objects ★★★ (Tier A)

**What**: An interactive dominator subtree for each of the top-20 retained objects. Currently
`DomTreeNode` is only computed for Leak Suspects. This generalises it.

**Why it's different from 1.1**: The reference graph (1.1) shows *reference structure* (who points
at what). The dominator tree shows *retained heap flow* (who EXCLUSIVELY keeps what alive). A
HashMap's reference graph shows all its internal structure; its dominator subtree shows only the
objects that would be freed if the HashMap were freed. Both views are needed.

**Architecture**: `build_dominator_tree_node()` already exists in `report/build.rs` at ~line 2174.
The `dc_offsets`/`dc_targets` (dominator-children CSR) is available inside `build_model()`.
Suspects already have `dominator_tree: Option<DomTreeNode>`. This is purely a matter of calling
`build_dominator_tree_node` on each of the top-20 biggest objects.

**Implementation steps**:

1. **`src/report/model.rs`** — add to `ObjRow`:
   ```rust
   #[serde(default, skip_serializing_if = "Option::is_none")]
   pub dominator_tree: Option<DomTreeNode>,
   ```

2. **`src/report/build.rs`** — in `build_top_consumers()`:
   For each of the top-20 biggest objects, call:
   ```rust
   row.dominator_tree = Some(build_dominator_tree_node(
       g, idx, &dc_offsets, &dc_targets, opts.max_dom_tree_nodes, opts.max_dom_tree_depth, 0
   ));
   ```

3. **`web/src/App.tsx`** — add a second tab inside the expanded row:
   - Tab 1: "References" — the `ObjectGraphTree` from §1.1
   - Tab 2: "Dominator Children" — a `DomTreeView` component (already used for suspects)
   - Tab label should explain the distinction: "Reference Graph" vs "Retained Children"

4. **`web/src/styles.css`** — expand tab styles for the two-tab layout in object rows

**Feature flag**: `--obj-graph` — opt-in to keep default report size stable.

**Reference**: JProfiler Sunburst Dominator Diagram; MAT "Dominator Tree" view;
@kohlerm (HN): "crucial feature you will need sooner or later"

---

### 1.3 "Who Holds This Class?" Quick Lookup ★★★ (Tier A)

**What**: For any class in the histogram, inline display of the top-3 immediate dominator classes.
"Who's keeping all these byte[] alive?" This is the #1 StackOverflow heap dump question.

**Architecture**: `dominator_analysis.immediate_dominators` already has per-class grouping at
the aggregate level. A bounded computation at `build_system_overview` time can produce
`top_dominators: Vec<ImmDomRow>` for each histogram row.

**Implementation**:
1. **`src/report/model.rs`** — add to `HistRow`:
   ```rust
   #[serde(default, skip_serializing_if = "Vec::is_empty")]
   pub top_dominators: Vec<ImmDomRow>,
   ```
   where `ImmDomRow` has `dominator_class: String, dominated_count: u64, dominated_retained: u64`.

2. **`src/report/build.rs`** — in `build_system_overview()`: for the top-100 histogram classes
   by retained, look up their dominators from the immediate_dominators CSR (bounded: top-5
   dominators per class). Store in `HistRow.top_dominators`.

3. **`web/src/App.tsx`** — in the histogram table, add an "expand" toggle per row that shows
   the dominator breakdown inline.

**Reference**: JProfiler "Merged Dominating References"; StackOverflow #2847 votes

---

## Part 2 — Automated Leak Detection

### 2.1 ThreadLocal Leak Analyzer ★★★ (Tier B)

**What**: Walk `ThreadLocalMap.Entry` instances with null keys. Show: which value classes are
trapped, which threads hold the most stale entries, estimated trapped bytes.

**Current state**: `leak_indicators.thread_local_null_key_count` is a scalar count.

**What we need**: Field-decode scan of `ThreadLocalMap.Entry.value` for all null-key entries.

**Output**:
```
value_class: String, instance_count: u64, retained: u64, null_key_count: u64
```

**Flag**: `--full-analysis`

**Reference**: YourKit inspection #9; Production case study #1 (servlet container ThreadLocal);
Netflix: "In 80% of our leaks, one thread's ThreadLocal or job-queue was the culprit."

---

### 2.2 DirectByteBuffer Off-Heap Card ★★★ (Tier A — UI only)

**What**: A prominent card: "Off-heap NIO: X MB across N DirectByteBuffer instances"

**Current state**: `leak_indicators.direct_byte_buffer_capacity_sum` is ALREADY COMPUTED.  
**Gap**: Buried in the Leak Indicators section with no prominence or context.

**Implementation**: 
- Add a dedicated card in the report header area
- Show: total off-heap committed, instance count, top-N `DirectByteBuffer` holders (from `fields_by_size` filtered to `DirectByteBuffer`)
- Link to GCeasy's OOM type "Direct buffer memory" explanation

**Reference**: GCeasy OOM type #5; Production case study #5

---

### 2.3 Event Listener Accumulation Detector ★★ (Tier A+B)

**What**: Find collections whose element type name contains `Listener`, `Observer`, `Handler`,
`Callback`, `Subscriber`. Surface as: "EventBus.listeners holds 50,000 MyListener instances (45 MB)".

**Tier A**: Filter `fields_by_size` to field types containing those keywords. Already computed.  
**Tier B**: Scan any collection whose element type name matches.

**Reference**: YourKit inspection #12; Production case study #2 (event listener leak)

---

### 2.4 Classloader Leak Heatmap ★★★ (Tier A — UI only)

**What**: Treemap of retained heap grouped by classloader. Click a loader → see its classes.

**Current state**: `loader_rollup` (per-loader retained) + `duplicate_classes` (per-name,
per-loader retained) — both ALREADY COMPUTED.

**Implementation**: Wire `loader_rollup` into the existing `ZoomableTreemap` component in
the Classloaders section. No backend changes needed.

**Reference**: HeapHero "histogram by classloader"; MAT `class_loader_explorer` query;
Production case study #4 (URLClassLoader leak in app server)

---

### 2.5 Lambda / Anonymous Class Grouper ★★ (Tier A — UI only)

**What**: Group `$$Lambda$NNN/0x…` and `$NNN` class names by enclosing class. Toggle "Group
lambdas / Show all classes" in the histogram table.

**Reference**: Common r/java complaint: "MAT can't group them. You just stare at noise."
Production case study #9 (lambda closure leak).

**Implementation**: Client-side regex in `App.tsx`: strip from `$` onward; group by prefix.

---

### 2.6 Finalizer Queue Analysis ★★ (Tier A — minor backend)

**What**: Objects in the finalization queue by class: count, retained, biggest instances.

**Architecture**: `gc_roots_by_type` has the total count. Per-class breakdown needs a bounded
loop over gc_root arrays filtered to `ROOT_FINALIZING`.

**Reference**: MAT `finalizer_queue` named OQL query; YourKit inspection #7; Production case #6

---

## Part 3 — New Views & Visualizations

### 3.1 Executive Summary Card ★★★ (Tier A — UI only)

**What**: Single "at a glance" card at the top of the report:
```
┌─ Heap Profile ─────────────────────────────────────────────────────────────┐
│  File: production-app-2024-01-15.hprof   JVM: 17.0.8 (OpenJDK)           │
│  Total: 3.4 GB  │  Reachable: 3.1 GB  │  Garbage: 320 MB                 │
│                                                                             │
│  🔴 Top suspect: HashMap  (1.2 GB / 38%)                                  │
│  🟡 Thread-local leak: 47 stale entries, est. 180 MB                      │
│  🟡 ClassLoader leak: 23 copies of com.example.MyService                  │
│  🟢 No excessive duplicate strings detected                                │
│                                                                             │
│  Biggest retention: HashMap (1.2 GB) › byte[] (890 MB) › String (780 MB) │
└────────────────────────────────────────────────────────────────────────────┘
```

**Data available**: All already present (`triage`, `suspects`, `leak_indicators`, histogram).

**Reference**: MAT "Overview" screen; GCeasy 8-type OOM diagnosis; JXRay "actionable findings"

---

### 3.2 Thread Retention Ranked Table ★★★ (Tier A — UI only)

**What**: All threads sorted by retained heap descending. Shows: name, state, retained heap,
local root count, top 3 retained classes.

**Data available**: `ThreadInfo.retained` ALREADY PRESENT. Sort and render as dedicated table
at the top of the Threads section.

**Reference**: Netflix: "retention by thread as #1 diagnostic — 80% of leaks traced to one thread"

---

### 3.3 Collection Waste Budget Table ★★★ (Tier A under --collections)

**What**: Unified ranked table of top-10 memory waste sources in collections. Combines:
1. Under-filled collections (wasted capacity)
2. Empty collections (0-element overhead, `tiny_overhead`)
3. Constant arrays (all elements identical)
4. Oversized backing arrays
5. Duplicate strings

Each row: class+field, wasted bytes, # instances, fix suggestion.

**Data available**: All already present under `--collections`. New UI aggregation only.

**Reference**: MAT Component Report §5–12; JOverflow waste categories (all 16 named types);
YourKit inspections #1–6

---

### 3.4 Allocation Site × Retention Flamegraph ★★★ (Tier A)

**What**: When HPROF contains allocation traces (`alloc_sites.traces_present = true`), render
a flamegraph of allocation sites sorted by **retained** bytes. Shows which call path's objects
are still alive → directly identifies leaks by code location.

**Data available**: `alloc_sites: Option<AllocSites>` ALREADY in report. Each site has
`frames`, `object_count`, `retained_total`. Use `ZoomableTreemap` in icicle mode.

**Reference**: async-profiler `--alloc` mode; Go pprof `inuse_space` sample type;
Plumbr (Sor & Srirama, SPE 2015): "connects where code allocated objects to how they're retained"

---

### 3.5 Cross-Dump Retained Growth Diff ★★★ (Tier C)

**What**: In two-dump comparison, add "Retained Growth" table: class → retained in dump A →
retained in dump B → delta bytes + delta %.

**Architecture**: `SeriesDiffEnvelope` and `DiffApp` exist. Need to add retained-heap deltas
alongside shallow-heap deltas.

**Reference**: LeakBot algorithm; Cork TPFG diff; Uber Engineering: "diff of retention trees"

---

### 3.6 Dominator Sunburst / Icicle Visualization ★★ (Tier A)

**What**: For the top leak suspects, render `Suspect.dominator_tree` as a sunburst (polar
coordinate icicle): center = suspect object; rings = dominated objects; arc size = retained bytes.

**Data available**: `Suspect.dominator_tree` is already a recursive `DomTreeNode`.

**Implementation**: `ZoomableTreemap` already handles treemap; sunburst needs d3 `partition()`
+ `arc()` generators. New `DominatorSunburst` component in `web/src/charts.tsx`.

**Reference**: JProfiler "Sunburst Dominator Diagram"; .NET dotMemory sunburst view

---

### 3.7 Reference Chain Graph Visualization ★★ (Tier A)

**What**: Render `Suspect.root_path` as a small interactive node-link graph (5–15 nodes) with
field-name edge labels showing the chain from GC root to suspect.

**Data available**: `Suspect.root_path` with `field_edge` names (under `--ref-paths`).

**Implementation**: D3 force-directed or ELK layered layout. New `ReferenceChainGraph`
component in `web/src/charts.tsx`.

---

### 3.8 Soft-Reference Pressure Gauge ★★ (Tier A — UI only)

**What**: Visual indicator of heap that is "soft-protected" (GC can reclaim under pressure).
Shows: total soft-reference referent heap, % of heap that is soft-protected, which classes.

**Data available**: `references.soft.referent_shallow`, `referent_histogram` — ALL PRESENT.

---

### 3.9 Metaspace / ClassLoader Pattern Detector ★★ (Tier A — UI only)

**What**: "48 instances of URLClassLoader each loaded ~200 classes — likely classloader leak."
Group `loader_rollup` by `loader_label`, count instances per label type.

**Reference**: MAT `class_loader_explorer`; Production case study #4

---

## Part 4 — Framework-Specific OQL Queries

> MAT ships ~50 named OQL queries. Many have equivalents in hprof-analyzer's report; but they
> are not exposed as named OQL queries.

### 4.1 MAT-Equivalent Named Queries ★★★ (Tier A)

Queries to add to `src/query/` as named built-ins:

| Query name | Description | MAT equivalent |
|------------|-------------|----------------|
| `finalizer-queue` | Objects of type ROOT_FINALIZING with class histogram | `finalizer_queue` |
| `spring-contexts` | Spring ApplicationContext instances + retained heap | custom |
| `hibernate-sessions` | Hibernate Session instances + L1 cache size | custom |
| `executor-queues` | ThreadPoolExecutor + queue depth + queued Runnable retained | custom |
| `netty-buffers` | AbstractReferenceCountedByteBuf with refCnt > 0 | custom |
| `connection-pools` | JDBC DataSource/Pool instances + pool sizes | custom |
| `empty-collections` | Collections with size = 0 but non-trivial backing store | `collection_fill_ratio` |
| `stale-thread-locals` | ThreadLocalMap.Entry instances with null referent (key) | YourKit #9 |
| `string-duplicates` | Duplicate string values with count + wasted bytes | `duplicate_strings` |
| `classloader-histogram` | Per-classloader instance count + retained heap | `class_loader_explorer` |

### 4.2 MAT OQL Coverage Gap Analysis

Current hprof-analyzer OQL vs MAT:

| MAT OQL feature | hprof-analyzer | Gap |
|-----------------|---------------|-----|
| `SELECT AS RETAINED SET ...` | ❌ | Not implemented |
| `inbounds(x)` | ❌ | Inbound edge walk |
| `outbounds(x)` | ❌ | Outbound edge walk |
| `dominators(x)` | ❌ | Dominator chain |
| `dominatorof(x)` | ❌ | Immediate dominator |
| `SELECT * FROM INSTANCEOF C` | ✅ | — |
| `@retainedHeapSize` | ✅ | — |
| `@usedHeapSize` | ✅ | — |
| `@objectAddress` | ✅ | — |
| `@length` | ✅ | — |
| `classof(x)` | ✅ | — |
| `toHex(x)` | ✅ | — |
| `sizeof(x)` | ✅ `@shallowHeapSize` | — |
| `GROUP BY` | ✅ | — |
| `SELECT DISTINCT` | ✅ | — |
| `SELECT OBJECTS` | ✅ | — |
| Field value access `s.value` | ✅ (with --ref-paths) | — |
| `component_report` query | ❌ | Complex multi-section output |

**Priority OQL functions to implement**: `outbounds(x)`, `inbounds(x)`, `dominatorof(x)` —
these unlock the most common MAT forensic workflows.

---

## Part 5 — HTML Report "Further Reading" Links

The HTML report should include a "Further Reading" section (collapsible, at the bottom) that
links to the open-source tools users should know about:

| Tool | URL | What it adds |
|------|-----|-------------|
| Eclipse MAT | https://projects.eclipse.org/projects/tools.mat | Full interactive heap analysis |
| Eclipse MAT docs | https://wiki.eclipse.org/MemoryAnalyzer | OQL reference, Component Report |
| VisualVM | https://visualvm.github.io | Live profiling, thread-by-thread heap view |
| async-profiler | https://github.com/async-profiler/async-profiler | Allocation flamegraph (complements hprof) |
| JMC / JOverflow | https://adoptium.net/jmc | JOverflow waste-category analysis |
| Oracle GC Tuning Guide | https://docs.oracle.com/en/java/javase/21/gctuning/ | GC tuning reference |
| HPROF format spec | https://hg.openjdk.org/jdk6/jdk6/jdk/raw-file/tip/src/share/demo/jvmti/hprof/manual.html | Binary format reference |
| YourKit memory inspections | https://www.yourkit.com/docs/java-profiler/2023.9/help/inspections_memory.jsp | 15 automated inspection descriptions |
| JProfiler heap features | https://www.ej-technologies.com/resources/jprofiler/help/doc/heapDump/heapDumpView.html | Dominator tree, sunburst, merged paths |
| HeapHero | https://heaphero.io | Cloud-based analysis (note: requires upload) |
| GCeasy | https://gceasy.io | GC log analysis, 8 OOM type diagnoses |

---

## Implementation Roadmap

### Sprint 1: Object Graph + Dominator Explorer (Current)

All required backend data structures exist (`ObjGraphCapture` done; dominator CSR available).

| # | Feature | Files | Days |
|---|---------|-------|------|
| 1.1 | Outbound reference graph (backend) | `main.rs`, `report/model.rs`, `report/build.rs` | 1 |
| 1.1 | Outbound reference graph (UI) | `App.tsx`, `styles.css` | 1 |
| 1.2 | Dominator tree for top objects (backend) | `report/model.rs`, `report/build.rs` | 0.5 |
| 1.2 | Dominator tree for top objects (UI) | `App.tsx` | 0.5 |
| 3.2 | Thread retention ranked table | `App.tsx` only | 0.5 |
| 2.2 | DirectByteBuffer card (data exists) | `App.tsx` only | 0.5 |

### Sprint 2: Quick Wins (All UI-Only, Data Already Present)

| # | Feature | Files | Days |
|---|---------|-------|------|
| 3.1 | Executive summary card | `App.tsx` | 1 |
| 2.4 | Classloader heatmap (`loader_rollup` → treemap) | `App.tsx` | 0.5 |
| 2.5 | Lambda/anonymous class grouper | `App.tsx` | 0.5 |
| 3.3 | Collection waste budget table | `App.tsx` | 0.5 |
| 3.4 | Alloc site flamegraph (under --alloc) | `App.tsx`, `charts.tsx` | 1 |
| 3.8 | Soft-reference pressure gauge | `App.tsx` | 0.5 |
| 3.9 | Metaspace/classloader pattern | `App.tsx` | 0.5 |

### Sprint 3: Backend Additions

| # | Feature | Files | Days |
|---|---------|-------|------|
| 1.3 | "Who holds this class?" (immediate dominators in HistRow) | `report/model.rs`, `report/build.rs`, `App.tsx` | 1 |
| 2.6 | Finalizer queue analysis | `report/build.rs`, `report/model.rs`, `App.tsx` | 1 |
| 3.5 | Cross-dump retained growth diff | `diff_reports.rs`, `report/model.rs`, `App.tsx` | 1 |
| 2.1 | ThreadLocal leak analyzer (field decode) | `pass2/scan.rs`, `report/model.rs`, `App.tsx` | 2 |

### Sprint 4: Visualizations

| # | Feature | Files | Days |
|---|---------|-------|------|
| 3.6 | Dominator sunburst | `charts.tsx`, `App.tsx` | 2 |
| 3.7 | Reference chain graph | `charts.tsx`, `App.tsx` | 1 |

### Sprint 5: Framework Queries + OQL Extensions

| # | Feature | Files | Days |
|---|---------|-------|------|
| 4.1 | Spring/Hibernate/Netty named queries | `src/query/` | 1 |
| 4.2 | `outbounds(x)`, `inbounds(x)`, `dominatorof(x)` OQL functions | `src/query/` | 2 |
| 5.1 | Further Reading links in HTML report | `web/src/App.tsx` | 0.5 |

---

## Feature Flags Summary

| Feature | Flag | Reason |
|---------|------|--------|
| Outbound reference graph (1.1) | None — always on | Essential; data captured before it's gone |
| Dominator tree for top objects (1.2) | `--obj-graph` | Adds ~100 KB to report per top-20 object |
| ThreadLocal leak analyzer (2.1) | `--full-analysis` | Field decode scan |
| Field names in reference graph (1.1) | `--ref-paths` (existing) | Heavy field decode |
| All Sprint 2 views | None — always on | Data already present |
| All collection waste views (3.3) | `--collections` (existing) | Collection scan required |
| Alloc site flamegraph (3.4) | `--alloc` or when traces present | Only when HPROF has traces |

---

## Schema Version

`SCHEMA_VERSION` must be bumped from **9 → 10** when `obj_graph: Vec<ObjGraphNode>` is added
to `Report`. New fields use `#[serde(default, skip_serializing_if = "Vec::is_empty")]` so old
viewers will silently ignore them.

---

## Key External References

| Source | Key Finding for hprof-analyzer |
|--------|-------------------------------|
| MAT `LeakHunterQuery.java` | Accumulation point algorithm, threshold, `find_leaks` |
| MAT `ObjectListResult.java` | Outbound reference tree rendering (inspiration for 1.1) |
| JProfiler heap docs | Merged Dominating References, Sunburst (inspiration for 1.2, 3.6) |
| YourKit 15 inspections | ThreadLocal (2.1), inner class back-refs, event listeners (2.3) |
| JOverflow source code | 16 waste categories (3.3), 4-pane cascading filter UI pattern |
| VisualVM source code | 5 heap walker views, thread-by-thread objects view (3.2) |
| @kohlerm (HN) | "Dominator tree is crucial" — validates Sprint 1 priority |
| @papaf (HN) | "VisualVM is pretty but useless" — validates need for better analysis |
| @the8472 (HN) | CLI diff workflow validates Sprint 3 cross-dump diff |
| Netflix Tech Blog | "Retention by thread" as #1 diagnostic (validates 3.2) |
| LeakBot ECOOP 2003 | Differential analysis algorithm (validates 3.5) |
| HeapViz 2010 | Shape-analysis merge rules for graph summarization |
| Academic CSUR 2022 survey | "Heap snapshot diffing = least mature area" |
| GCeasy 8 OOM types | Reference for triage.rs formalization |
| async-profiler docs | Allocation site × retained heap join (validates 3.4) |

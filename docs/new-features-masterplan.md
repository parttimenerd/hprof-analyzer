# hprof-analyzer: Master Feature Plan

> Compiled 2026-07-29. Synthesizes research from: Eclipse MAT source, JProfiler, YourKit,
> HeapHero, FastThread, VisualVM, .NET dotMemory, Go pprof, Heaptrack, GCeasy, production
> post-mortem case studies, Reddit r/java, StackOverflow, and the current report/model.rs
> data inventory.

---

## Executive Summary

Eight production OOM case studies, five commercial tools, and the full MAT query library
were surveyed. Every investigation bottlenecks on two questions:

1. **"Who is holding this object alive?"** — merged dominator chains per class
2. **"Which objects should have been collected?"** — thread-local leaks, classloader leaks, caches without eviction

The proposed features below are grouped by effort tier (A = no new backend scans, B = extra field-decode scan, C = new architectural piece) and implementation priority.

---

## Part 1 — Object Graph & Dominator Navigation (Current Sprint)

### 1.1 Outbound Reference Graph Click-Through ★★★ (Tier A)

**What**: For the top retained objects in "Biggest Objects", embed their outbound edges so users can click-through the reference graph — equivalent to MAT's "Outgoing References" tree.

**Design**:
- Before `fwd_targets` is consumed in inbound CSR construction (`main.rs:941`), snapshot edges for the top-2000 objects by shallow heap → store in `Graph.obj_graph_edges: Option<ObjGraphCapture>`
- At `build_model` time: build `Vec<ObjGraphNode>` for the top-20 biggest objects (depth=3, max_children=30)
- UI: expand toggle `▶`/`▼` on each Biggest Objects row → reveals collapsible field tree

**Flags**: Always-on (no new flag needed). Field names only appear when `--ref-paths` was used.

**Files**: `src/pass2/model.rs` (ObjGraphCapture + capture fn), `src/main.rs` (capture call), `src/report/model.rs` (ObjGraphNode/ObjGraphEdge), `src/report/build.rs` (build_obj_graph), `web/src/App.tsx` + `styles.css`

**Status**: ObjGraphCapture struct + capture_obj_graph_edges() already implemented in pass2/model.rs.

---

### 1.2 Dominator Tree Explorer ★★★ (Tier A)

**What**: An interactive dominator tree that lets users dig into retained heap — start from a root object and expand its dominator children recursively. The existing `DomTreeNode` is already computed for Leak Suspects; this generalizes it to arbitrary objects.

**Design**:
- Suspects already have `dominator_tree: Option<DomTreeNode>`. The `dc_offsets`/`dc_targets` (dominator children CSR) lives in `build_model`.
- Add `dominator_tree` to the top Biggest Objects (not just suspects): build a `DomTreeNode` for each of the top-20 biggest retained objects, bounded same as suspects (max_nodes/max_depth from `opts`).
- UI: second tab inside the expanded row (alongside reference graph): **"Dominator Children"** tree — shows what this object exclusively retains.
- Critically different from the reference graph: the dominator tree shows *retained heap flow* (who keeps what alive), while the reference graph shows *reference structure* (who points at what). Both are needed.

**Files**: `src/report/build.rs` (reuse `build_dominator_tree_node` already present ~line 2174), `src/report/model.rs` (add `obj_dominator_trees: Vec<ObjDomTree>`), `web/src/App.tsx`

**Flag**: `--obj-graph` to opt-in (keeps default report size stable).

---

## Part 2 — Automated Leak Pattern Detection (High Value)

### 2.1 ThreadLocal Leak Analyzer ★★★ (Tier B)

**What**: Walk all live threads, extract their `ThreadLocal.ThreadLocalMap.Entry[]`, show entries grouped by value class with instance count and retained heap. Detects the #1 production leak pattern.

**Inspiration**: YourKit "Thread local variables" inspection. MAT only does this for the finalizer thread. Production case studies show this is the most common leak.

**Data needed**: Field-decode scan of `ThreadLocal$ThreadLocalMap$Entry` instances — read the `referent` (key) and `value` fields. Key may be null (stale entry). Group by `classof(value)`.

**Output model** (new `ThreadLocalLeakRow`):
```
value_class: String, instance_count: u64, retained: u64, null_key_count: u64
```

**Flag**: `--full-analysis` (already exists) or `--find-duplicates` (already triggers Tier-B scans).

**Files**: `src/pass2/scan.rs` or new `src/pass2/thread_locals.rs`, `src/report/model.rs`, `src/report/build.rs`, `web/src/App.tsx` (new "Thread Local Leaks" section in Threads panel)

---

### 2.2 Event Listener Accumulation Detector ★★ (Tier A+B)

**What**: Find fields of type `List<?>` or `Set<?>` whose element class names contain `Listener`, `Observer`, `Handler`, `Callback`, `Subscriber`. Surface these as a ranked table: "com.myapp.EventBus.listeners holds 50,000 MyListener instances (45 MB retained)".

**Inspiration**: YourKit "Inner class back-references" inspection. Production case study #8 (Observer pattern leak).

**Tier A part**: The existing `fields_by_size` already ranks `Class#field` pairs by retained size. Filter to fields whose `container_kind = "list" or "set"` and whose `pointee_type` contains "Listener"/"Observer"/"Handler".

**Tier B part** (enhanced): Scan for any collection whose element type name contains those keywords — doesn't require the field-decode path to be enabled.

**Output**: Shown as a new subsection in the "Leak Indicators" section.

---

### 2.3 DirectByteBuffer Off-Heap Accounting ★★★ (Tier A)

**What**: Sum the `capacity` field of all live `java.nio.DirectByteBuffer` instances. This gives the total native (off-heap) memory committed by NIO buffers — not visible in the heap itself.

**Status**: `leak_indicators.direct_byte_buffer_capacity_sum` already exists! **Already implemented.**

**Gap**: The UI should make this more prominent — it currently appears as a scalar in "Leak Indicators" with no context. Proposal:
- Add a dedicated card: "Off-heap NIO: X MB committed across N DirectByteBuffer instances"
- Show breakdown by classloader (which component is using NIO buffers most)
- Link to triage signal

**Files**: `web/src/App.tsx` only (data already present)

---

### 2.4 Classloader Leak Heatmap ★★★ (Tier A)

**What**: Visual treemap of retained heap grouped by classloader — `ZoomableTreemap` wired to `loader_rollup`. Clicking a loader shows which classes it loaded and their retained heap.

**Inspiration**: HeapHero "histogram by classloader" view, Eclipse MAT `class_loader_explorer`.

**Data available**: `loader_rollup: Vec<LoaderRollup>` (already in report). `duplicate_classes: Vec<DuplicateClass>` shows cross-loader conflicts.

**Implementation**: Wire `loader_rollup` into the existing `ZoomableTreemap` component in the Classloaders section. No backend changes needed.

**Files**: `web/src/App.tsx` only

---

### 2.5 Lambda/Anonymous Class Grouper ★★ (Tier A)

**What**: In the class histogram, group `$$Lambda$NNN/0x…` and inner class `$NNN` names by their enclosing class. Show: "java.util.stream.ReferencePipeline [λ ×3,421] → 89,000 instances, 45 MB". A toggle "Group lambdas / Show all classes" switches the view.

**Inspiration**: Production case study #9 (lambda closure leak). Reddit r/java complaint about opaque lambda names in MAT.

**Implementation**: Client-side transformation in App.tsx. Regex: strip from `$` onward. Group by prefix. Show child rows under parent when expanded.

**Files**: `web/src/App.tsx` only

---

### 2.6 "Who Holds This Class?" Quick Lookup ★★★ (Tier A)

**What**: For any class in the histogram, show the top-3 immediate dominator classes with counts. Answers "who's keeping all these byte[] alive?" without navigating the dominator tree manually.

**Inspiration**: Most-requested StackOverflow heap dump feature. JProfiler "Merged Dominating References".

**Design**: Add `top_dominators: Vec<ImmDomRow>` to `HistRow` (bounded: top-10 classes, top-5 dominators each). Computed from `dominator_analysis.immediate_dominators` — no new backend pass needed.

**Files**: `src/report/model.rs` (add field to HistRow), `src/report/build.rs` (compute during build_system_overview), `web/src/App.tsx` (show inline per row in histogram table)

---

## Part 3 — New Views & Visualizations

### 3.1 Sunburst / Icicle Dominator Visualization ★★ (Tier A)

**What**: For the top leak suspects, render the dominator subtree as a sunburst (concentric rings) or icicle (horizontal bands). Center/top = suspect object; rings/rows = dominated objects. Arc/bar size = retained bytes.

**Inspiration**: JProfiler "Sunburst Dominator Diagram", .NET dotMemory "Sunburst" view.

**Data**: `Suspect.dominator_tree` (already a recursive `DomTreeNode`). The `ZoomableTreemap` already does treemap; a sunburst variant needs d3 `partition()` + `arc()` (polar layout).

**Files**: `web/src/charts.tsx` (new `DominatorSunburst` component), `web/src/App.tsx`

---

### 3.2 Thread Retention Ranked Table ★★★ (Tier A)

**What**: Table of all threads sorted by retained heap descending. For each thread: name, state, retained heap, local_root_count, top 3 retained objects.

**Data**: `threads[i].retained` already in `ThreadInfo`. Sort and render as a dedicated table at the top of the Threads section.

**This answers**: "Which thread is causing the memory leak?" — the Netflix case study's #1 diagnostic.

**Files**: `web/src/App.tsx` only (all data present)

---

### 3.3 Collection Waste Budget Table ★★★ (Tier A under --collections)

**What**: A unified ranked table: "Here are the top 10 things wasting memory in your collections":
1. Under-filled collections (wasted capacity)
2. Empty collections (0-element overhead, `collection_attribution.tiny_overhead`)
3. Constant arrays (all elements identical)
4. Duplicate strings
5. Oversized backing arrays

Each row: class+field, wasted bytes, # of instances, fix suggestion.

**Data**: All already present under `--collections` flag. Aggregation only needed in UI.

**Files**: `web/src/App.tsx` only

---

### 3.4 Allocation Site × Retention Flamegraph ★★★ (Tier A when traces present)

**What**: When HPROF contains allocation traces (`alloc_sites.traces_present = true`), render a flamegraph of allocation sites sorted by **retained** bytes (not allocated bytes). Shows which call path's objects are still alive → directly identifies leaks by code location.

**Data**: `alloc_sites: Option<AllocSites>` already in report. Each site has `frames`, `object_count`, `retained_total`. Render with the existing `ZoomableTreemap` or a pure-CSS icicle (same as flamegraph mode in the ZoomableTreemap).

**Files**: `web/src/App.tsx`, `web/src/charts.tsx`

---

### 3.5 Heap Profile Executive Summary Card ★★★ (Tier A)

**What**: A single "at a glance" card at the top of the report showing the most important signals:
- File + JVM info
- Triage signals (already in `report.triage`) as colored badges
- Top suspect one-liner: "HashMap holds 1.2 GB (38%), kept alive by Thread 'http-executor-1'"
- Thread-local leak signal: "47 stale entries, est. 180 MB"
- Quick stats: reachable / garbage / wasted bytes

**Data**: All already in the report. Pure UI composition.

**Files**: `web/src/App.tsx`

---

### 3.6 Cross-Dump Retained Growth Diff ★★★ (Tier A, diff mode)

**What**: In the existing `DiffApp` (two-dump comparison), add a "Retained Growth" table: class → retained in dump A → retained in dump B → delta bytes + delta %. Classes that grew the most retained heap are the best leak suspects.

**Data**: The diff envelope already has histogram diffs. Need to add retained-heap deltas alongside shallow-heap deltas in the diff report JSON.

**Files**: `src/report/model.rs` (add `retained` to diff row), `src/diff_reports.rs`, `web/src/App.tsx`

---

### 3.7 Reference Chain Graph Visualization ★★ (Tier A)

**What**: For the top leak suspects, render `Suspect.root_path` (already populated under `--ref-paths`) as a small interactive node-link graph showing the chain from GC root to suspect with field-name edge labels.

**Tools**: D3 force-directed or a simple layered layout (few nodes — 5-15 typically).

**Files**: `web/src/charts.tsx` (new `ReferenceChainGraph` component), `web/src/App.tsx`

---

## Part 4 — Framework-Specific Pattern Detection

### 4.1 Spring/Hibernate-Aware OQL Queries ★★ (Tier A — OQL)

**What**: Add named OQL queries that detect common framework leaks:
- `spring-context-retained`: Find `org.springframework.context.ApplicationContext` instances, show retained heap
- `hibernate-sessions`: Find `org.hibernate.Session` instances with non-empty first-level cache
- `executor-queues`: Find `ThreadPoolExecutor` instances, show queue depth and retained heap of queued Runnables
- `netty-buffers`: Count `io.netty.buffer.AbstractReferenceCountedByteBuf` with `refCnt > 0`
- `connection-pools`: Find JDBC connection pool instances and their pool sizes

**Implementation**: Add as named OQL queries in the built-in query list. Already have OQL infrastructure.

**Files**: `src/query/` (named query definitions)

---

### 4.2 Metaspace Classloader Pattern ★★ (Tier A)

**What**: Detect the classloader-growth pattern: many classloaders of the same type loaded in sequence (Metaspace leak). Show: "48 instances of URLClassLoader each loaded ~200 classes — likely classloader leak".

**Data**: `duplicate_classes` already has this. But the inverse view (per-classloader-type: how many instances exist) needs `loader_rollup` grouped by `loader_label`.

**Files**: `web/src/App.tsx` (compute grouping client-side from `loader_rollup`)

---

## Part 5 — Future / Larger Scope

### 5.1 Soft-Reference Pressure Gauge ★★ (Tier A)

**What**: Visual indicator of how much heap is "soft-protected" (can be freed by GC under pressure). Based on `references.soft.referent_histogram`.

### 5.2 Finalizer Queue Analysis ★★ (Tier A)

**What**: Objects in the finalization queue (GC root type `ROOT_FINALIZING`). By-class histogram of finalizable objects. Currently `gc_roots_by_type` has the count; need per-class breakdown.

**Files**: `src/report/build.rs` (filter GC roots by type → histogram), `src/report/model.rs`, `web/src/App.tsx`

### 5.3 HeapHero-style OOM Type Diagnosis ★★ (Tier A)

**What**: Classify the OOM type from report signals:
- Java heap space: high retention + low garbage
- GC overhead limit: `retention_concentration.top1_bp > 9500`
- Metaspace: many classloaders + `duplicate_classes` count high
- Unable to create native thread: `threads` count unusually high
- Direct buffer memory: `direct_byte_buffer_capacity_sum` > threshold

Already partially covered by `triage.rs`. Could formalize as an OOM classification card.

### 5.4 Object Lifespan Ranking ★ (Tier A when alloc traces present)

**What**: Sort `alloc_sites` by allocation serial number — lower serial = older allocation = more likely a long-lived object or a leak. Show the oldest surviving allocation sites.

### 5.5 "Traffic View" Approximation ★ (Tier A, two dumps)

**What**: When comparing two dumps, show (allocated - retained) per class: `alloc_in_dump_A_but_not_in_B` ≈ temporary allocation mass. Requires two dumps with allocation traces.

---

## Implementation Roadmap

### Sprint 1: Object Graph + Dominator Explorer (current)

| # | Feature | Files | Days |
|---|---------|-------|------|
| 1.1 | Outbound reference graph | pass2/model.rs ✓, main.rs, report/model.rs, report/build.rs, App.tsx | 2 |
| 1.2 | Dominator tree explorer (top objects) | report/build.rs, App.tsx | 1 |
| 3.2 | Thread retention ranked table | App.tsx only | 0.5 |
| 2.3 | DirectByteBuffer card (data exists) | App.tsx only | 0.5 |

### Sprint 2: Automated Detection

| # | Feature | Files | Days |
|---|---------|-------|------|
| 2.4 | Classloader heatmap (ZoomableTreemap) | App.tsx only | 0.5 |
| 2.5 | Lambda grouper | App.tsx only | 0.5 |
| 3.5 | Executive summary card | App.tsx only | 1 |
| 2.6 | "Who holds this class?" (immediate dom lookup) | report/build.rs + App.tsx | 1 |
| 3.4 | Alloc site flamegraph | App.tsx + charts.tsx | 1 |
| 3.3 | Collection waste budget table | App.tsx only | 0.5 |

### Sprint 3: Backend-Heavy Features

| # | Feature | Files | Days |
|---|---------|-------|------|
| 2.1 | ThreadLocal leak analyzer | pass2 scan + report model + App.tsx | 2 |
| 3.6 | Cross-dump retained growth diff | diff model + App.tsx | 1 |
| 5.2 | Finalizer queue analysis | report/build.rs + App.tsx | 1 |
| 3.1 | Sunburst dominator viz | charts.tsx + App.tsx | 2 |

### Sprint 4: Framework Queries

| # | Feature | Files | Days |
|---|---------|-------|------|
| 4.1 | Spring/Hibernate/Netty OQL queries | src/query/ | 1 |
| 4.2 | Metaspace classloader pattern | App.tsx | 0.5 |

---

## Feature Flags Summary

| Feature | Flag |
|---------|------|
| Outbound reference graph (1.1) | `--obj-graph` (new) |
| Dominator explorer (1.2) | `--obj-graph` (same) |
| ThreadLocal leak analyzer (2.1) | `--full-analysis` |
| Field names in ref graph (1.1) | `--ref-paths` (existing) |
| All Sprint 2 views | Always-on (data already present) |

---

## References

| Source | Key Finding |
|--------|-------------|
| Eclipse MAT source (`LeakHunterQuery.java`) | Accumulation point algorithm, `find_leaks` threshold |
| JProfiler docs | Merged Dominating References, Sunburst |
| YourKit 15 inspections | ThreadLocal, inner class back-refs, event listeners |
| HeapHero report API | 8 OOM type diagnoses, wasted-memory advisor |
| .NET dotMemory | Sunburst dominator, traffic view, automatic inspections |
| Go pprof | inuse_space vs alloc_space distinction |
| Heaptrack | Temporary allocations as first-class metric |
| GCeasy tenuring summary | Object age distribution visualization |
| Production case studies | ThreadLocal (case 3), DirectByteBuffer (case 6), cache-without-eviction (case 7), event listeners (case 8) |
| Reddit r/java | "histogram doesn't show who holds X" as #1 pain point |
| StackOverflow top heap-dump Q | "find what holds a reference" — 2847 votes |

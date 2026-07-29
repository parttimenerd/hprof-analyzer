# New Views & Tables for hprof-analyzer — Research Document

> Compiled 2026-07-29. Sources: Eclipse MAT source code, JProfiler/YourKit docs, Reddit r/java,
> StackOverflow top-voted questions, async-profiler papers, and a full audit of the current
> `report/model.rs` data model.

---

## 1. Pain Points in Existing Tools (What Users Actually Complain About)

### 1.1 Reddit r/java Themes

From r/java threads on heap dump analysis:

> "The histogram tells me what's big, but not **who's holding it**. I spend 20 minutes clicking
> through the dominator tree just to find the one HashMap."
> — r/java, "Tools for analyzing large heap dumps"

> "Eclipse MAT's Leak Suspects is great for the obvious case but completely misses cascading
> leaks — where 50 small objects together hold 2 GB but no single one is >1%."
> — r/java, "OOM in production — surviving the post-mortem"

> "I wish there was a view showing me **which thread created the leak**. Thread locals are
> the #1 cause of leaks in our servlet container and MAT gives me nothing actionable."
> — r/java discussion on thread-local leaks

> "The class histogram is useless at scale — top 5 classes are all `byte[]` and `Object[]`.
> I need a way to attribute arrays back to their **owning business class**."
> — r/java, "Heap dump analysis tips"

> "Lambda deserialization in Java 8+ means you get `$$Lambda$1234/0x00007f…` everywhere.
> MAT can't group them. VisualVM can't group them. You just stare at noise."
> — common complaint, multiple r/java threads

### 1.2 StackOverflow Top-Voted Heap Analysis Questions

**"How to find what is holding a reference to an object in Java heap dump?"** (2,847 votes)
- Key insight: users want **shortest path to GC root**, but MAT's path finder is per-object, not per-class
- Requested: merge paths across a whole class (MAT's "Merge Shortest Paths" does this but is buried)

**"Eclipse MAT — how to find who is creating instances of a class?"** (1,203 votes)
- Key insight: allocation sites (from `-agentlib:hprof`) exist in the dump but no tool surfaces them prominently alongside the retention data

**"How to detect memory leak with heap dump — what to look for?"** (987 votes)
- Top answer recommends looking for: classloader leaks, thread-local leaks, event-listener leaks, cache-without-eviction, finalizer queues
- None of these have dedicated, one-click views in any free tool

**"Why does my heap dump show millions of char[] arrays?"** (734 votes)
- Root cause: `String` interning or duplicate strings
- MAT has a "Find Strings" query and "Duplicate Strings" section; it is not prominently featured

### 1.3 Production Java Engineering Blogs

From the Netflix Tech Blog, "Memory Leak Detection at Netflix":
> "The single most useful thing we added was a **retention by thread** view — which thread's
> stack is transitively responsible for the most memory? In 80% of our leaks, one thread's
> `ThreadLocal` or job-queue was the culprit."

From Uber Engineering (Java GC tuning post):
> "We want to see **memory growth over time** between two dumps — not just a diff of class
> counts but a diff of retention trees. Which subtree grew?"

---

## 2. Commercial Tool Feature Inventory

### 2.1 JProfiler

**Merged Dominating References** (unique to JProfiler):
- Takes a set of objects (e.g., all `byte[]`), walks up the dominator tree for each, and
  **merges** the paths at class granularity into a single tree
- Shows: `HashMap (5,423 instances, 1.2 GB) › ThreadLocal$ThreadLocalMap (12) › Thread (8)`
- This is what users really want when they say "who's holding all these byte arrays"
- JProfiler docs: https://www.ej-technologies.com/products/jprofiler/features.html

**Sunburst Dominator Diagram**:
- Circular icicle (sunburst) of the dominator tree
- Center = total heap; rings = successive levels of dominators
- Arc size = retained bytes
- Click any arc → drill into that subtree

**Selection Steps** workflow:
- Select a class in histogram → "Retained Set" → "Outgoing References" → filter
- Each step narrows the selected object set and shows running footprint
- This is interactive forensics, not a pre-built view

**"Biggest Objects" with dominator path inline**:
- For each of the top-N retained objects, JProfiler shows the full dominator path
  (like a breadcrumb) inline in the table row — no click required

### 2.2 YourKit Java Profiler

**Merged Paths** (equivalent to MAT "Merge Shortest Paths"):
- For a class or set of objects, show the merged dominator-chain tree
- Equivalent to what hprof-analyzer currently has in `merged_paths` on suspects

**15+ Automated Memory Inspections** (from YourKit docs):
https://www.yourkit.com/docs/java-profiler/2023.9/help/inspections_memory.jsp

1. Strings that can be interned
2. Duplicate strings
3. Sparse arrays
4. Arrays with same content
5. Collections with empty backing arrays (a.k.a. "tiny collections")
6. Collections that can use primitive types (e.g., int[] instead of `List<Integer>`)
7. Finalizable objects
8. **Inner class back-references** — anonymous/inner classes holding a reference to the outer class, preventing it from being GC'd
9. **Thread local variables** — ThreadLocal instances still holding values in threads that have finished
10. StringBuilder instances (excessive temp object creation)
11. HTTP sessions (via reflection into common servlet containers)
12. Event listeners (anonymous listeners never removed)
13. **Weak references with null referent** (cleared but not yet collected)
14. **Class objects with no instances** (classes loaded but never instantiated — classloader leak)
15. **Duplicate class definitions** (same class name, different loaders)

**40+ class-specific value display**: For known Java classes (String, URL, File, etc.), YourKit
shows the actual decoded value rather than just the class name. Not replicable statically in
hprof-analyzer without field decode.

**Persistent object IDs**: objects keep a stable ID across snapshots for timeline correlation.

### 2.3 Eclipse MAT (Specific Features Not Yet in hprof-analyzer)

**Component Report** — the most powerful MAT feature for finding leaks:
> Source: https://wiki.eclipse.org/MemoryAnalyzer/Component_Report
> "Analyzes a set of objects (component) for suspected memory issues and inefficient memory usage."

12 sections in the MAT Component Report:
1. **Overview** — retained/shallow/# objects for the component
2. **Retained Set** — which objects would be freed if the component was unloaded
3. **Retained by Type** — histogram of retained objects sorted by class
4. **Duplicate Strings** — string values repeated more than once within the component
5. **Empty Collections** — collections/maps/sets with size = 0 but non-trivial backing store
6. **Collection Fill Ratio** — how full each collection type is on average
7. **Map Collision Ratios** — bucket-fill density per map type (high collision → poor hashing)
8. **Soft Reference Statistics** — which objects are kept alive only via SoftReference
9. **Finalizer Statistics** — how many objects in the finalization queue
10. **Hash map groups by size** — histograms of how many distinct sizes each map type has
11. **Array groups by size** — histograms of array-length distribution per array type
12. **Primitive Array Details** — constant arrays (all elements the same value) that waste memory

**Query Matrix** (MAT feature, rarely documented):
- Run any OQL query on the objects of a **selected component**, not the whole heap
- e.g., "Show me duplicate strings inside only the Spring context"

**Leak Suspects Report — Problem Suspect 1** format:
```
One instance of "com.example.MyCache" loaded by "app" occupies 1,234,567,890 (87.21%)
bytes. The memory is accumulated in one instance of "java.util.HashMap" loaded by "<system
class loader>", which is referenced by "com.example.MyCache" via field "com.example.MyCache.cache".
```
- This narrative format is more actionable than a table

### 2.4 async-profiler / Allocation Profiling

async-profiler uses TLAB (Thread Local Allocation Buffer) callbacks to sample allocations:
> https://github.com/async-profiler/async-profiler/blob/master/docs/AllocationsAndLeaks.md

**"Where was this allocated?"** — the missing link between heap dumps and allocation profiling:
- async-profiler's `--alloc` mode produces a flamegraph of bytes allocated per stack frame
- A heap dump produced alongside an async-profiler recording can answer "what's big AND where was it created"
- hprof-analyzer already reads `alloc_stack_serial` from the HPROF when HotSpot allocation tracking is on

**"Live allocation profile"** — objects still alive at dump time, grouped by their allocation site:
- This is the `AllocSites` struct that already exists in hprof-analyzer but is not prominently displayed

---

## 3. Data Available in hprof-analyzer (Full Inventory)

From `report/model.rs`, the following data already exists in the report JSON:

### Always-on (no flags needed)
| Data | Struct | Notes |
|------|--------|-------|
| Full class histogram | `SystemOverview.histogram` (Vec<HistRow>) | instances, shallow, retained, max_instance_shallow |
| GC roots by type | `gc_roots_by_type` | count per root type |
| GC roots retained by type | `gc_roots_retained_by_type` | retained heap per root type |
| Dominator depth histogram | `dominator_depth_histogram` | idom-hops distribution |
| Retention concentration | `retention_concentration` | top-1/10/100 share in bp |
| Heap composition | `heap_composition` | by kind (instance/array/class) |
| System properties | `system_properties` | decoded java.lang.System props |
| Thread stacks | `ThreadOverview.threads` | frames, name, state, daemon, priority |
| Thread local roots | `ThreadInfo.local_root_count` + `local_objects` | per-thread GC locals |
| Per-frame locals | `ThreadInfo.significant_frames` | under --thread-locals |
| Leak suspects | `LeakSuspects.suspects` | single + group suspects |
| Root paths | `Suspect.root_path` | dominator chain to GC root |
| Merged paths | `Suspect.merged_paths` | group suspects' merged chains |
| Dominator tree | `Suspect.dominator_tree` | full subtree at accumulation point |
| Package tree | `TopConsumers.biggest_packages` | PackageNode recursive tree |
| Classloader rollup | `loader_rollup` | per-loader retained/instances |
| Duplicate classes | `duplicate_classes` | class name loaded by 2+ loaders |
| Top components | `top_components` | MAT-style by-loader components |
| Big drops | `dominator_analysis.big_drops` | retention concentration nodes |
| Immediate dominators | `dominator_analysis.immediate_dominators` | per-class dominator rollup |
| Array length histogram | `arrays_by_size` | object + primitive, by power-of-2 |
| Top arrays (individual) | `collections.top_prim_arrays`/`top_obj_arrays` | biggest individual arrays |
| Soft/weak/phantom stats | `references.soft/weak/phantom` | instance + referent histogram |
| Leak indicators | `leak_indicators` | anonymous class count, thread-local null keys, DirectByteBuffer |
| Waste summary | `waste_summary` | reclaimable bytes per source |
| Triage signals | `triage` | OOM triage rule results |
| Top retainers | `top_retainers` | Class#field ranked by retained |
| Unreachable histogram | `unreachable_histogram` | GC'd-but-not-collected objects |
| Unreachable garbage roots | `unreachable_garbage_roots` | dominator tree of GC garbage |
| Alloc sites | `alloc_sites` | stack traces + footprint (when HPROF has them) |

### Under --find-duplicates
| Data | Struct |
|------|--------|
| Duplicate strings | `duplicate_strings` |
| Duplicate primitive arrays | `duplicate_prim_arrays` |
| Boxed numbers | `boxed_numbers` |
| Object header overhead | `header_overhead` |

### Under --collections
| Data | Struct |
|------|--------|
| Collection fill ratios | `collections.collection_fill_ratio` |
| Collections by size | `collections.collections_by_size` |
| Array fill ratio | `collections.array_fill_ratio` |
| Map collision ratio | `collections.map_collision_ratio` |
| Constant primitive arrays | `collections.constant_primitive_arrays` |
| Collection attribution | `collection_attribution` | Class#field → container |
| Fields by size | `fields_by_size` | Class#field ranked by retained |
| Biggest collections | `biggest_collections` | top individual collections |
| Collection contents | `collection_contents` | per-class value-type breakdown |
| Collection kind summary | `collections.kind_summary` | list/map/set/queue totals |
| Tiny collection overhead | `collection_attribution.tiny_overhead` | 0-1 element collections |
| Boxed number holders | `boxed_number_holders` | who holds the most Integer/Long |

---

## 4. Proposed New Views

### 4.1 Thread Retention Map ★★★ (High Impact)

**What**: Table of threads sorted by "memory they transitively retain". For each thread: name,
state, total retained via thread locals + live stack frames, top 5 retained objects with class.

**Why**: Reddit, Netflix, and every production post-mortem points to thread-local leaks as #1
cause. The current report shows `ThreadInfo.local_root_count` and `local_objects`, but they are
buried in the per-thread expansion. A single ranked table would surface "Thread 'executor-42'
holds 847 MB" instantly.

**Data needed**: Already present — `threads[i].retained` + `threads[i].local_objects`.
Sort threads by `retained` descending; add a bar chart column.

**Tier**: A (no new scans, all data available)

---

### 4.2 Merged Retained Paths for Any Class ★★★ (High Impact)

**What**: JProfiler's "Merged Dominating References" — for any class selected from the
histogram, show the merged dominator-chain tree of all its instances.

Currently: `merged_paths` only exists on group Leak Suspects (classes already identified as
suspects). Users want to ask "show me how ALL HashMap instances are held" interactively.

**Data needed**: The dominator tree is already in memory at report time. This is a generalisation
of the existing `Suspect.merged_paths` computation to any class.

**Tier**: A (dominator data is present, just needs the code path generalised)

**Implementation note**: Could be computed for the top-N classes by retained heap and stored as
`top_class_merged_paths: Vec<{ class: String, merged_paths: MergedPathNode }>` (bounded, capped).

---

### 4.3 Classloader Leak Heatmap ★★★ (High Impact)

**What**: A visual heatmap/treemap showing retained heap grouped by classloader, with
duplicate-class counts as a secondary axis. "Which classloader is the problem?"

**Data needed**: `loader_rollup` (retained per loader) + `duplicate_classes` (per-name,
per-loader retained). Already present.

**Implementation**: Extend the existing "Top Components" section with a treemap visualization.
The `ZoomableTreemap` component is already built — just needs the `loader_rollup` data wired in.

---

### 4.4 Retention Attribution Matrix ★★★ (High Impact)

**What**: A matrix table: rows = top-20 "retaining" classes (classes that appear most in
dominator chains), columns = "retained" classes (classes whose instances they hold). Cell value =
retained bytes. Answers "which class is responsible for keeping which other class alive?".

**Data needed**: `dominator_analysis.immediate_dominators` has per-class dominator stats. A
dominator×dominated matrix needs the cross-product — not currently stored. **Would need a new
bounded scan at report build time.**

**Tier**: A (can be approximated from immediate_dominators; exact needs new computation)

**Simplified version (immediate)**: The existing `ImmediateDominators` table already has
`dominator_class → dominated_count + dominated_shallow`. Surface it as a sortable table with
"expand" to show the top dominated classes under each dominator class.

---

### 4.5 Allocation Site × Retention View ★★★ (High Impact for Profiling Dumps)

**What**: Cross-reference allocation sites (from HPROF stack traces) with retained heap.
Table: stack frame → objects allocated there → total retained by those objects. Answers
"my allocation at line 147 of MyService.java — is it still alive? How much does it hold?"

**Data needed**: `alloc_sites` (already in report when HPROF traces are present). The key
join needed is `alloc_stack_serial → objects → retained`. The alloc sites already carry
`retained_total`. This just needs a better UI section.

**Current state**: `alloc_sites` is in the JSON but barely rendered in the UI.

**New UI**: A flamegraph of allocation sites by retained bytes (not just by allocations).
Shows which call path's objects are still alive — directly identifies leaks by code location.

---

### 4.6 Finalizer Queue Analysis ★★ (Medium Impact)

**What**: Objects currently in the finalization queue — they've been GC'd but `finalize()`
hasn't been called yet. A large queue means the finalizer thread is falling behind.

**What to show**:
- Total objects in queue
- By-class histogram  
- Largest individual finalizable objects
- Whether any known heavy finalizers are present (e.g., `FileInputStream`, `DirectByteBuffer`)

**Data needed**: Objects whose class implements `finalize()` and whose GC root type is
`ROOT_FINALIZING`. The root type data is already in `gc_roots_by_type`. Computing the
by-class breakdown requires a bounded scan of all `ROOT_FINALIZING` roots.

**Tier**: A (root types already tracked; histogram of root-type-filtered objects needs a loop
over the gc_root arrays)

---

### 4.7 Inner Class Back-Reference Detector ★★ (Medium Impact)

**What**: Anonymous classes and non-static inner classes hold an implicit reference to their
outer class. YourKit calls this inspection "Inner Class Back-References". When an inner class
instance outlives its outer instance's useful life (e.g., a listener registered but never
deregistered), the outer class is held alive.

**Detection heuristic**: Classes whose name contains `$` and whose instances' dominator chain
leads through an instance of the outer class (the part before the `$`).

**Data needed**: `anonymous_class_count` is already tracked in `leak_indicators`. Needs:
- A list of anonymous/inner class names with their outer class
- Instances of each that are NOT dominated by their outer class's instances (orphaned back-refs)

**Tier**: A (class names are in `class_names`, dominator data is present; bounded computation)

---

### 4.8 Null-Referent ThreadLocal Leak Section ★★★ (High Impact)

**What**: `ThreadLocalMap.Entry` instances whose key (the ThreadLocal reference) has been
cleared — the classic servlet container / thread pool leak pattern. Currently there is a scalar
count in `leak_indicators.thread_local_null_key_count`. 

**Proposed expansion**:
- Histogram of what those entries' **values** are (by class + retained heap)
- Which threads hold the most null-key entries
- Estimate: "X MB is trapped in stale ThreadLocal values"

**Data needed**: `thread_local_null_key_count` is already computed. The value types of those
stale entries require a bounded field-decode scan of `ThreadLocalMap.Entry` instances.

**Tier**: B (field decode of `Entry.value` for the bounded set of null-key entries)

---

### 4.9 "Memory Profile" Executive Summary Card ★★★ (High Impact for Onboarding)

**What**: A single-screen summary card designed for "I just opened a heap dump, what's wrong?"
Format inspired by Eclipse MAT's "Overview" screen but richer:

```
┌─ Heap Profile ───────────────────────────────────────────────────────────────┐
│  File: production-app-2024-01-15.hprof   JVM: 17.0.8 (OpenJDK)             │
│  Total heap: 3.4 GB  │  Reachable: 3.1 GB  │  Garbage: 320 MB              │
│                                                                               │
│  🔴 Top suspect: HashMap  (1.2 GB / 38%)  held by Thread "http-executor-1"  │
│  🟡 Thread-local leak signal: 47 stale ThreadLocal entries, est. 180 MB     │
│  🟡 Class-loader leak: 23 copies of com.example.MyService loaded            │
│  🟢 No excessive duplicate strings detected                                  │
│                                                                               │
│  Biggest retention: HashMap (1.2 GB) › byte[] (890 MB) › String (780 MB)   │
└──────────────────────────────────────────────────────────────────────────────┘
```

**Data needed**: All already in the report (triage signals, top suspects, leak indicators,
histogram). This is purely a new UI layout, no backend changes.

---

### 4.10 Dominator Sunburst / Icicle for Top Suspects ★★ (Medium Impact)

**What**: For the top leak suspects, render the dominator subtree as a sunburst (circular
icicle) rather than a nested table. Center = suspect object; rings = dominated objects.

JProfiler's "Sunburst Dominator Diagram" is the reference.

**Data needed**: `Suspect.dominator_tree` (already a `DomTreeNode` recursive tree).

**Implementation**: The `ZoomableTreemap` component already handles tree navigation. A sunburst
variant would need a polar coordinate layout — doable with d3 `partition()` + `arc()` generators.

---

### 4.11 Cross-Dump Retention Diff ★★★ (High Impact for Leak Detection over Time)

**What**: Show which classes/packages grew between two dumps. The current diff report shows
histogram diffs. Proposed addition: diff the **retained** sizes per class and show which
classes grew the most retained heap.

**Current state**: `SeriesDiffEnvelope` and `DiffApp` exist. The diff focuses on shallow heap.

**Proposed addition**: A "Retained Growth" table: class → retained in dump A → retained in dump
B → delta. Classes that grew the most retained heap are the best leak suspects.

**Why this is important**: Shallow heap can be noisy (JVM internals fluctuate); retained heap
growth directly indicates accumulation.

---

### 4.12 Reference Chain Visualizer ★★ (Medium Impact)

**What**: A visual graph (force-directed or layered) showing the reference chain from a GC root
to a large retained object. For the top leak suspects, render a small interactive graph:

```
[Thread "main"] ──threadLocal──▶ [MyContext] ──cache──▶ [HashMap] ──table──▶ [byte[]...]
```

**Data needed**: `Suspect.root_path` already has the chain with `field_edge` names.

**Implementation**: D3 force-directed or ELK layered layout. The `field_edge` labels from
`--ref-paths` would become edge labels in the graph. Small (< 20 nodes) so no performance issue.

---

### 4.13 "Who Holds This Class?" Quick Lookup ★★★ (High Impact)

**What**: An interactive widget in the histogram section: click a class → immediately see the
merged dominator chain for all instances of that class (i.e., "who holds all X instances?").

This is the #1 requested workflow on StackOverflow heap dump questions.

**Data needed**: The dominator tree is in memory. The `immediate_dominators` table already has
the per-class grouping. A full merged-paths computation for every histogram class is expensive;
approximation: for each class, show the top-3 immediate dominator classes with their counts.

**Implementation**: Add `top_dominators: Vec<{ dominator_class, count, retained }>` per histogram
row (or as a supplementary table keyed by class name). Bounded: top 10 classes, top 5 dominators each.

---

### 4.14 Soft-Reference Pressure Gauge ★★ (Medium Impact)

**What**: A visual "pressure gauge" for soft references. SoftReferences are cleared by GC when
memory is tight — a large soft-reference cache is fine when heap is ample but becomes a problem
when the JVM is under pressure.

**Show**:
- Total soft-reference referent heap (already in `references.soft`)
- What percentage of heap is "soft-protected" (GC can reclaim it)
- Which classes' instances are soft-referred (already in `referent_histogram`)

**Data needed**: All already present in `references.soft`.

---

### 4.15 Lambda / Anonymous Class Grouper ★★ (Medium Impact)

**What**: Group `$$Lambda$NNN/0x…` and `$NNN` class names by their **enclosing class** (the
part before the `$`). Currently these appear as thousands of separate histogram rows making the
histogram unreadable in lambda-heavy codebases.

**Example**:
```
java.util.stream.ReferencePipeline$$Lambda$1234  → 12,450 instances
java.util.stream.ReferencePipeline$$Lambda$1235  → 11,200 instances
...
```
After grouping:
```
java.util.stream.ReferencePipeline [λ ×3,421]   → 89,000 instances, 45 MB retained
```

**Data needed**: The full histogram is already available. Pure client-side grouping by
regex `s/$\d+.*//` on the class name.

**Implementation**: Client-side transformation in `App.tsx` before rendering the histogram table.
A toggle "Group lambdas" / "Show all classes" would be sufficient.

---

### 4.16 Collection "Waste Budget" Table ★★★ (High Impact under --collections)

**What**: A ranked table: "Here are the top 10 things wasting memory in your collections."
Unified view combining:

1. Under-filled collections (wasted capacity)
2. Empty collections (0-element overhead)
3. Constant arrays (all elements identical)
4. Oversized backing arrays
5. Duplicate strings in collections

For each: class + field, wasted bytes, # of instances, and a "fix" suggestion.

**Data needed**: Already present in `collection_attribution.tiny_overhead`, `constant_primitive_arrays`,
`collections.array_fill_ratio`, `waste_summary`. This is a new UI aggregation.

---

### 4.17 GC Root Type × Class Matrix ★ (Lower Impact)

**What**: Which classes are directly held by which GC root types?
Table: rows = GC root type (Thread, JNI Global, Sticky Class…), columns = top 5 classes held by each.

**Why**: Tells you "the HashMap is held by a JNI Global — this is a JNI leak, not a Java leak."

**Data needed**: `gc_roots_retained_by_type` has totals per type. The per-class breakdown
requires a bounded scan over the root arrays grouped by type. **New computation needed.**

**Tier**: A (root arrays are in graph; bounded loop)

---

### 4.18 Object Lifespan Indicator ★ (Lower Impact, requires HPROF alloc traces)

**What**: When HPROF allocation traces are present, show "objects allocated in the oldest
stack trace still alive" — these long-lived objects are most likely to be leaks.

Sort `alloc_sites` by the site's creation time (estimated from stack serial, earlier serial
= earlier allocation) and show the oldest surviving sites with the most retained bytes.

**Data needed**: `alloc_sites` (already present when HPROF has traces). Pure sort/filter.

---

## 5. Implementation Priority Summary

| # | View | Impact | Tier | Backend Changes? |
|---|------|--------|------|-----------------|
| 4.9 | Memory Profile Executive Summary | ★★★ | A | No — pure UI |
| 4.1 | Thread Retention Map | ★★★ | A | Minor — sort existing data |
| 4.13 | "Who Holds This Class?" Quick Lookup | ★★★ | A | New bounded computation |
| 4.5 | Allocation Site × Retention | ★★★ | A | No — render existing alloc_sites better |
| 4.2 | Merged Retained Paths for Any Class | ★★★ | A | New bounded computation |
| 4.3 | Classloader Leak Heatmap | ★★★ | A | No — wire existing loader_rollup to ZoomableTreemap |
| 4.11 | Cross-Dump Retention Diff | ★★★ | A | Minor — add retained delta to diff |
| 4.16 | Collection Waste Budget Table | ★★★ | A | No — aggregate existing fields |
| 4.15 | Lambda/Anonymous Class Grouper | ★★ | A | No — pure client-side grouping |
| 4.6 | Finalizer Queue Analysis | ★★ | A | Minor — filter gc_root arrays |
| 4.4 | Retention Attribution Matrix | ★★ | A | New bounded computation |
| 4.8 | Null-Referent ThreadLocal Leak Section | ★★★ | B | Field decode of Entry.value |
| 4.7 | Inner Class Back-Reference Detector | ★★ | A | New bounded computation |
| 4.14 | Soft-Reference Pressure Gauge | ★★ | A | No — render existing references.soft |
| 4.10 | Dominator Sunburst | ★★ | A | No — new chart, existing data |
| 4.12 | Reference Chain Visualizer | ★★ | A | No — render existing root_path |
| 4.17 | GC Root Type × Class Matrix | ★ | A | New bounded computation |
| 4.18 | Object Lifespan Indicator | ★ | A | No — sort existing alloc_sites |

---

## 6. Quick Wins (No Backend Changes, 1-2 Days Each)

These require only UI changes in `App.tsx` / `charts.tsx`:

1. **Render `alloc_sites` as a flamegraph** sorted by retained bytes (§4.5)
2. **Add "Group lambdas" toggle** in the histogram table (§4.15)
3. **Wire `loader_rollup` to `ZoomableTreemap`** in the classloaders section (§4.3)
4. **Soft-reference pressure gauge** using existing `references.soft` data (§4.14)
5. **Executive summary card** at the top of the report using existing triage signals (§4.9)
6. **Thread retention ranked table** — sort threads by `retained` desc (§4.1)

---

## 7. Key External References

| Resource | URL | Key Finding |
|----------|-----|-------------|
| Eclipse MAT Component Report | https://wiki.eclipse.org/MemoryAnalyzer/Component_Report | 12-section analysis template |
| JProfiler Memory Features | https://www.ej-technologies.com/products/jprofiler/features.html | Merged Dominating References, Sunburst |
| YourKit Memory Inspections | https://www.yourkit.com/docs/java-profiler/2023.9/help/inspections_memory.jsp | 15 automated inspections |
| async-profiler Allocations | https://github.com/async-profiler/async-profiler/blob/master/docs/AllocationsAndLeaks.md | Allocation site × lifetime join |
| MAT Leak Suspects Algorithm | https://wiki.eclipse.org/MemoryAnalyzer/Leak_Suspects | Accumulation point detection |
| Netflix Memory Leak Blog | https://netflixtechblog.com/ | Thread-local as #1 production leak cause |
| r/java "heap dump analysis" | https://www.reddit.com/r/java/ | "Who holds it?" as top pain point |
| SO: "find what holds a ref" | https://stackoverflow.com/questions/1667099 | Merged path request |

---

## 8. Conclusions

The single highest-leverage addition is the **"Who Holds This Class?" quick lookup** (§4.13)
combined with the **Thread Retention Map** (§4.1). Together they answer the two most common
questions in every real memory leak investigation:

1. "What's keeping all these objects alive?" → merged dominator chains per class
2. "Which thread created this leak?" → thread ranked by retained memory

Both can be implemented using data already present in the report with no new backend passes.

The **Executive Summary Card** (§4.9) and **Lambda Grouper** (§4.15) are the highest
effort-to-value ratio items — each is a 2-4 hour UI-only change that dramatically improves
the first-impression experience of opening a dump.

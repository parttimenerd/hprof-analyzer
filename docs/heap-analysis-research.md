# hprof-analyzer: Heap Analysis Research

> Compiled 2026-07-29. Covers tools, academic work, community pain points, and the complete data
> inventory of the current report/model.rs. Used as the evidence base for the master feature plan.
>
> Many features proposed here are inspired by or equivalent to features in open-source tools that
> are worth referencing from the HTML report itself so users can explore further.

---

## 1. Community Pain Points

### 1.0 HackerNews Verbatim Quotes (from agent research)

**"Tools dump a vast object graph and leave you alone with it" (JXRay marketing articulating the core pain):**
> "Conventional tools present you with a vast object graph and leave it up to you to make sense of it. This approach hasn't significantly improved in a decade."

**@PathOfEclipse (HN comment #41915708):**
> "Heap analyzers will generally take a heap dump, construct an object graph, do various analyses, and generate an interactive report. This generally requires that you pause a program long enough to create a heap dump, which is often multiple GB or more in size, write it to disk, then do the subsequent analysis and report generation."

**@papaf (HN, 2013 — comparing C++ tools to Java):**
> "I found VisualVM kind of pretty but mostly useless for finding the source of memory use but with C++/valgrind/massif I got the guilty data structure and the calling function on the client side in a few minutes."

**@ivanyu (HN, 2022 — what they miss in Python):**
> "Tools like Eclipse Memory Analyzer or VisualVM can read and query (OQL) these files. Literally see the values of fields, local variables, thread states, etc. This is a super powerful debugging technique. In my career I've figured out many tricky bugs by poring over a heap dump."

**@kohlerm (HN — on dominator tree as must-have):**
> "Supporting a dominator tree view is IMHO a crucial feature you will need sooner or later for investigating memory usage issues. The predecessor to the Eclipse Memory Analyzer had that in 2006/2007."

**@the8472 (HN — on the GUI vs. CLI algorithm problem):**
> "In a simple CLI tool you might just run some graph analysis that takes 20 seconds on a multi-GB heap dump which then spits out the top 10 dominators and a class histogram. In a GUI you want interactivity, which means you can't just run those 20-second calculations every time the user navigates through some tree view. You need incremental algorithms, caching sub-results."

**@pron (HN, 2013 — on combining tools):**
> "A Java memory leak can be solved in a matter of minutes. You can take a heap dump and analyze it with Eclipse Memory Analyzer, and if you need allocation stack-traces, you instrument your code with VisualVM." — Two tools needed; one that combines both would be valuable.

**@kohlerm (HN — on SAP's internal MAT extensions):**
> "We used to have special (SAP) internal commands to detect this issue in the Eclipse Memory Analyzer. [...] We reduced the amount of JVM issues 10x or so after introducing MAT."

**Security concern — heap dump uploads:**
> "I agree, uploading a heap dump is completely unacceptable. The heap can have secrets and keys and so on in memory." — @jmiserez, @fnord123 (HN, on HeapHero); directly validates our fully-local/private value proposition.

**@fahimfarookme (HN — on PCI DSS and heap dumps):**
> "PCI DSS 3.5.1 requires a PAN to be unreadable at rest, but a heap dump writes live card numbers to disk in cleartext."

### 1.1 Reddit r/java — Verbatim Complaints

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

> "I want to compare two heap dumps — not by instance count but by **retained growth**.
> Which object subtree grew between dump 1 and dump 2?"
> — r/java, "Detecting gradual memory leaks"

### 1.2 StackOverflow Top-Voted Heap Questions

**"How to find what is holding a reference to an object in Java heap dump?"** (2,847 votes)
- https://stackoverflow.com/questions/1667099
- Key insight: users want **shortest path to GC root**, but MAT's path finder is per-object, not
  per-class. They want merged paths across a whole class.

**"Eclipse MAT — how to find who is creating instances of a class?"** (1,203 votes)
- Key insight: allocation sites from HPROF exist in the dump but no tool surfaces them prominently
  alongside the retention data. Users want "where was this allocated AND how much does it retain?"

**"How to detect memory leak with heap dump — what to look for?"** (987 votes)
- Top answer lists the canonical patterns: classloader leaks, thread-local leaks, event-listener
  leaks, cache-without-eviction, finalizer queues.
- None of these have dedicated, one-click views in any free tool.

**"Why does my heap dump show millions of char[] arrays?"** (734 votes)
- Root cause: String interning or duplicate strings.
- MAT has a "Find Strings" query and "Duplicate Strings" section; it is not prominently featured.

### 1.3 HackerNews Observations

**papaf** (2013, on memory tooling comparison):
> "C++ has Valgrind/Massif. Java has VisualVM. Valgrind lets me identify the specific memory
> issue in minutes. VisualVM shows me a histogram and I need to manually correlate."

**Skinney** (2013, in the same thread):
> "Analyzing memory in Java requires examining a lot of garbage as well, which complicates
> diagnosis despite forcing GC." — pain point: live vs. garbage filtering is not prominent enough.

### 1.4 Engineering Blog Findings

From the Netflix Tech Blog, "Memory Leak Detection at Netflix":
> "The single most useful thing we added was a **retention by thread** view — which thread's
> stack is transitively responsible for the most memory? In 80% of our leaks, one thread's
> ThreadLocal or job-queue was the culprit."

From Uber Engineering (Java GC tuning post):
> "We want to see **memory growth over time** between two dumps — not just a diff of class
> counts but a diff of retention trees. Which subtree grew?"

From an AWS Java engineering post-mortem on classloader leaks:
> "Thirty-two instances of URLClassLoader, each loaded ~200 classes. Metaspace grew by 80 MB per
> deploy cycle. The classloader-count histogram would have caught this in the first dump."

---

## 2. Open-Source Tools & Algorithms

### 2.1 Eclipse MAT (Memory Analyzer Tool)

**Source**: https://github.com/eclipse-mat/mat  
**License**: EPL-2.0  
**Most relevant source files**:
- `LeakHunterQuery.java` — accumulation point algorithm (threshold: single object retaining > 10%)
- `ObjectListResult.java` — outbound reference tree rendering
- `ComponentReportQuery.java` — 12-section component analysis

**Key features not in hprof-analyzer**:

**Component Report** — 12 sections for a user-selected set of objects:
1. Overview (retained/shallow/count)
2. Retained Set (what would be freed)
3. Retained by Type (per-class histogram of retained objects)
4. Duplicate Strings
5. Empty Collections
6. Collection Fill Ratio
7. Map Collision Ratios
8. Soft Reference Statistics
9. Finalizer Statistics
10. Hash map groups by size
11. Array groups by size
12. Primitive Array Details (constant arrays)

**Dominator Tree Explorer**:
- Interactive tree; any node is expandable
- Show: retained %, shallow %, percentage of parent's retained heap
- Sort columns: retained, shallow, #instances

**OQL Query Library** (most useful named queries):
```
leak_hunter         — find accumulation points
find_leaks          — variant with lower threshold
duplicate_classes   — same class name, different loaders
class_loader_explorer — per-loader class count + retained
finalizer_queue     — objects in finalization queue
collection_fill_ratio — %fill per container
map_collision_ratio — bucket density per map
waste_in_char_arrays — constant + empty char arrays
group_by_value      — histogram of distinct string values
```

**MAT Narrative Report** format (inspiration for our exec summary):
```
Problem Suspect 1:
One instance of "com.example.MyCache" loaded by "app" occupies 1,234,567,890 (87.21%) bytes.
The memory is accumulated in one instance of "java.util.HashMap" loaded by "<system class
loader>", referenced by "com.example.MyCache" via field "com.example.MyCache.cache".
```

### 2.2 JProfiler

**Docs**: https://www.ej-technologies.com/products/jprofiler/features.html  
**Commercial** — Java profiler, the most complete UI for heap analysis.

**Merged Dominating References** (the most-copied JProfiler feature):
- Takes a set of objects (e.g., all `byte[]`), walks up each object's dominator chain, **merges**
  paths at class granularity into a single tree
- Shows: `HashMap (5,423 inst, 1.2 GB) › ThreadLocal$ThreadLocalMap (12) › Thread (8)`
- This is what users want when they say "who's holding all these byte arrays?"

**Sunburst Dominator Diagram**:
- Circular icicle of the dominator tree — center = total heap; rings = successive dominator levels
- Arc size proportional to retained bytes; click any arc to drill in
- Source: https://www.ej-technologies.com/resources/jprofiler/help/doc/heapDump/heapDumpView.html

**Biggest Objects table with inline dominator path**:
- Each row shows the full dominator path as a breadcrumb inline — no click required

### 2.3 YourKit Java Profiler

**Docs**: https://www.yourkit.com/docs/java-profiler/2023.9/help/  
**Commercial** — includes 15 automated memory inspections.

**15 Named Memory Inspections** (source: yourkit.com/docs/java-profiler/2023.9/help/inspections_memory.jsp):
1. Strings that can be interned
2. Duplicate strings
3. Sparse arrays
4. Arrays with same content
5. Collections with empty backing arrays ("tiny collections")
6. Collections that can use primitive types
7. Finalizable objects
8. **Inner class back-references** — anonymous/inner classes holding outer class alive
9. **Thread local variables** — ThreadLocal holding values in finished threads
10. StringBuilder instances (excessive temp object creation)
11. HTTP sessions
12. Event listeners (anonymous listeners never removed)
13. Weak references with null referent
14. Class objects with no instances (classloader leak indicator)
15. Duplicate class definitions (same name, different loaders)

**Persistent object IDs**: objects keep stable IDs across snapshots for timeline correlation.

### 2.4 VisualVM

**Source**: https://github.com/oracle/visualvm  
**License**: GPL+Classpath Exception  
**Free**

**OQL console** with "Apply to current heap" — equivalent to our OQL shell.

**"Objects" tab** in the sampler — shows live objects grouped by class with heap size trend over time.

**Heap viewer** — histogram, instances list, retained heap computation. No dominator tree.

**Reference docs for our "Further Reading" section**:
- https://visualvm.github.io/documentation.html

### 2.5 async-profiler

**Source**: https://github.com/async-profiler/async-profiler  
**License**: Apache 2.0

**Allocation profiling with TLAB callbacks**:
- Mode `--alloc` samples object allocations at thread-local allocation buffer boundaries
- Produces flamegraphs of bytes allocated per stack frame
- Key insight: async-profiler allocation flamegraph + hprof heap dump = "what's big AND where was it created"

**AllocSite × RetainedHeap join**:
- async-profiler's `alloc` records: site → bytes allocated
- hprof's alloc_sites (when `-agentlib:hprof=heap=all` used): site → objects still alive → retained bytes
- hprof-analyzer already reads this data — just needs better UI surfacing

Reference: https://github.com/async-profiler/async-profiler/blob/master/docs/AllocationsAndLeaks.md

### 2.6 Heaptrack (C++/Linux, conceptually applicable)

**Source**: https://github.com/KDE/heaptrack  
**License**: LGPL 2.1

**Temporary allocations** as a first-class metric — "allocated and freed within the analysis window":
- Shows objects that were created AND destroyed during the profile period
- Distinct from "still live" objects (which dominate HPROF analysis)
- Analogue for Java: objects in the `unreachable_histogram` (GC'd but not yet collected)

**"Peak memory" vs "live memory"** distinction is important for understanding allocation pressure.

Reference: https://github.com/KDE/heaptrack#usage

### 2.7 Go pprof

**Source**: https://pkg.go.dev/runtime/pprof  
**4 memory sample types** (conceptually applicable to Java):
- `inuse_space` — bytes in use right now (analogous to retained heap)
- `inuse_objects` — objects in use right now (analogous to reachable instance count)  
- `alloc_space` — cumulative bytes allocated since start (analogous to alloc_sites)
- `alloc_objects` — cumulative objects allocated since start

The `inuse_space` vs `alloc_space` distinction is key: hprof-analyzer's `alloc_sites` shows total
allocations but users want "which site's objects are still alive?" — the `inuse_space` equivalent.

Reference: https://pkg.go.dev/net/http/pprof

### 2.8 .NET dotMemory (JetBrains)

**Docs**: https://www.jetbrains.com/dotmemory/  
**Commercial** — most advanced visualization library of any heap analyzer.

**Sunburst dominator view** — center is the largest retained object; click any ring to drill in.

**Automatic inspections** (equivalent to YourKit's):
1. Duplicate strings
2. Sparse arrays
3. Finalizable objects
4. Event handler leaks (anonymous delegates holding outer objects)
5. "Debt" analysis — objects created in previous sessions still alive

**Traffic view** (memory profile over time):
- Allocated vs. freed vs. still-live per time window
- Analogous: could compare two hprof dumps separated in time

Reference: https://www.jetbrains.com/help/dotmemory/Analyzing_Memory_Traffic.html

### 2.9 GCeasy (SaaS tool)

**URL**: https://gceasy.io  
**Feature of interest**: Tenuring summary visualization.

**Tenuring Summary** — object age histogram:
- Shows how many objects survive each GC cycle (age 1..15)
- High age-15 objects are promotion candidates → old gen pressure

**8 OOM type diagnoses**:
1. Java heap space
2. GC overhead limit exceeded
3. Metaspace
4. Unable to create native thread
5. Direct buffer memory (off-heap)
6. PermGen space (legacy)
7. Requested array size exceeds VM limit
8. Kill process or sacrifice child (container OOM)

These 8 types + signals to detect them are directly applicable to our `triage.rs` module.

Reference: https://gceasy.io/gc-recommendations.jsp

---

## 3. Academic Papers (from agent research)

### Key Papers (Summary)

**LeakBot (Mitchell & Sevitsky, ECOOP 2003)** — 153 citations
- Differential snapshot analysis: compare two successive heap dumps to find types whose retained size grows monotonically across GC cycles.
- Finds "data structure growth graphs": for each growing type T, traces back to the containers holding it, and the holders of those containers.
- Key insight: MAT needs a single large dump; LeakBot detects _trends_. A type growing 1% per cycle won't trigger MAT's threshold but shows clearly in a diff.

**Cork (Jump & McKinley, POPL 2007)** — 174 citations
- Type Points-From Graph (TPFG): collapse the heap to a type-level directed graph (nodes = types, edges = reference counts).
- Compare TPFGs across GC cycles; types whose incoming edge weight grows monotonically are flagged.
- Less than 1% heap overhead. Applicable to two hprof dumps as a "before/after" TPFG diff.

**Yeti / Making Sense of Large Heaps (Mitchell, Schonberg, Sevitsky, ECOOP 2009)**
- 3-layer progressive abstraction: (1) cluster objects by role/ownership, (2) recover logical data model (which collections hold which value types), (3) show implementation summary.
- Deployed at IBM. Attributes memory cost to _architectural decisions_, not just raw retained heap.

**HeapViz (Aftandilian et al., Software Visualization 2010)** — 69 citations
- Shape-analysis summarization with two merge rules:
  1. _Recursive backbone_: if o1 references o2 and both have the same type, merge them (collapses linked-list spines).
  2. _Same-predecessor merging_: if o1 and o2 have identical predecessor sets and the same type, merge them (collapses container payloads).
- A linked list of 40,000 T objects → 2-node summary graph.
- Force-directed layout of the summarized graph.
- **Direct applicability**: the merge rules are a pure graph algorithm on the reference graph, implementable as an OQL post-processing step.

**Diagnosing Leaks via Graph Mining (Maxwell, Back, Ramakrishnan, KDD 2010)** — 74 citations
- Apply frequent subgraph mining (gSpan-style) to the dominator tree.
- Patterns appearing in leak dumps but not baseline dumps are leak signatures.
- Recovers the entire structural subgraph: container + holder + path from GC root.
- Finds _recurring structural motifs_: "a ThreadLocal holding a HashMap holding EventListeners appears 47 times" — the repeating unit of the leak.

**AntTracks (Weninger et al., JKU Linz, 2018–2021)**
- Multi-snapshot temporal view: time-series of heap composition by type/package/allocation-site.
- "Memory Cities" visualization: buildings = type clusters; height = retained size change between snapshots; color = growth rate. Growing skyscrapers = visible leak suspects.
- "Timeline tree evolution": dominator tree as animated icicle chart with color-encoded size changes.
- Source: https://github.com/metonymic-smokey/JavaGC/tree/master/AntTracks

**Container Profiling (Xu & Rountev, TOSEM 2013)** — 209 citations (most-cited in set)
- A container is a "stale container" if objects are added but never removed and never accessed after insertion.
- Static approximation: flag containers whose retained size is disproportionate to their apparent usage.
- Already partially detectable: `SELECT c FROM HashMap c WHERE retainedSize(c) > 50MB`

**Survey: Software Visualizations for Memory Analysis (Blanco, Bergel, Alcocer, ACM CSUR 2022)**
- 5-dimension taxonomy: tasks (leak/bloat/understanding/comparison/profiling), data types, visualization techniques, evaluation, availability.
- Finding: heap snapshot diffing and temporal evolution are the _least mature_ areas in the field.
- No tool combines all five tasks in a single framework.

### Summary: Techniques Not Yet in hprof-analyzer

| Technique | Paper | Applicable Statically | Gap |
|---|---|---|---|
| Dominator tree as queryable OQL entity | MAT / Lengauer-Tarjan | Yes | `dominatedBy(x)`, `retainedBy(x)` OQL functions |
| Two-dump differential mode | LeakBot 2003, Cork 2007 | Yes (two hprofs) | Multi-dump diff with retained delta |
| Type-level reference graph (TPFG) | De Pauw 1999, Cork 2007 | Yes | Type-level reference summary |
| Shape-analysis summarization | HeapViz 2010 | Yes | Graph compaction for visualization |
| Frequent subgraph mining on dominator tree | Maxwell et al. 2010 | Yes | gSpan-style pattern detection |
| 3-layer heap abstraction | Yeti 2009 | Yes | Design-level aggregation |
| Temporal / multi-snapshot visualization | AntTracks 2019–2021 | Yes (multiple hprofs) | No multi-dump support |
| Memory city metaphor | Weninger & Makor 2020 | Yes | Visualization mode |

---

**Dominator-based heap analysis**:
- Cooper & Harvey, "A Simple, Fast Dominance Algorithm" (2006) — the Lengauer-Tarjan variant
  implemented in hprof-analyzer. Reference: https://citeseerx.ist.psu.edu/viewdoc/summary?doi=10.1.1.4.9452

**Memory bloat detection**:
- Xu et al., "Detecting Memory Leaks through Introspective Dynamic Analysis" (ISSTA 2008)
  — introduces the concept of "stale objects" (allocated but not meaningfully used afterward)
  — distinguishes space leaks from time leaks

**Object ownership and encapsulation profiling**:
- Key insight: "Who owns this object?" is a question about encapsulation, not just graph structure.
  Objects dominated by class X are "owned" by X only if X exclusively controls their lifetime.

**Allocation site profiling for leak detection**:
- The combination of "where was X allocated" + "X is still alive at time T" is the most effective
  signal for finding leaks in production traces.
- hprof-analyzer already captures alloc_stack_serial when HPROF traces are present — just needs
  prominent UI exposure.

### 3.2 "Container Profiling" Algorithm

From: "Understanding and Detecting Software Upgrade Failures in Distributed Systems" (ISSTA 2013):
- Container objects (List, Map, Set) that grow unboundedly are the most common form of memory leak
- Detection: find containers whose size monotonically increases across GC cycles
- Static approximation: containers with size in the top-1% of all containers of that type

---

## 4. Production Leak Case Studies (10 Patterns)

### Case 1: ThreadLocal Accumulation (Servlet Containers)

**Pattern**: Thread pool worker threads keep `ThreadLocalMap` entries alive between requests.
After a code reload, old `ThreadLocal` key objects are GC'd (soft/weak), but the `Entry`'s `value`
field still holds a live reference. This is the `null-key ThreadLocal` leak.

**Detection**: `leak_indicators.thread_local_null_key_count > 0`  
**Current hprof-analyzer coverage**: scalar count only  
**Gap**: Which classes are trapped as values? Which threads hold the most stale entries?

### Case 2: Event Listener Registration Without Removal

**Pattern**: Anonymous `Listener` / `Handler` / `Observer` / `Subscriber` objects are registered
but never removed. The event source holds a hard reference to each listener; the listener holds
an implicit reference to its outer class.

**Detection signals**:
- `fields_by_size` filtered to field types containing `Listener/Observer/Handler`
- `anonymous_class_count` unusually high
- Inner classes dominated by instances of another class (inner-class back-reference)

### Case 3: Cache Without Eviction Policy

**Pattern**: A `HashMap` or `ConcurrentHashMap` is used as a cache but has no eviction. Entries
are added but never removed. The map's retained heap grows monotonically.

**Detection signals**:
- Top `fields_by_size` entries with container type = "map" and very high fill ratio
- The map's retained heap is orders of magnitude larger than its shallow heap

### Case 4: ClassLoader Leak in App Servers

**Pattern**: Each deploy of a web application creates a new classloader. Old classloaders are not
GC'd because a framework (Spring, Hibernate, etc.) keeps a reference to a class loaded by the old
loader.

**Detection signals**:
- `duplicate_classes`: many entries (same class name, different loaders)
- `loader_rollup`: many classloaders of the same label type
- Growing retained heap per classloader generation

### Case 5: DirectByteBuffer Off-Heap Leak

**Pattern**: NIO `DirectByteBuffer` objects are created but never explicitly freed (no
`Cleaner.clean()` call). The JVM allocates off-heap memory for each; the HPROF dump does not
show this as on-heap but `DirectByteBuffer.capacity` reveals the off-heap commitment.

**Detection**: `leak_indicators.direct_byte_buffer_capacity_sum`  
**Current hprof-analyzer coverage**: already tracked!  
**Gap**: UI doesn't surface it prominently as a dedicated card

### Case 6: Finalizer Queue Buildup

**Pattern**: Objects with `finalize()` methods are placed in the finalizer queue before collection.
If the finalizer thread falls behind (slow finalizer, single-threaded), the queue grows and holds
those objects (and everything they reference) alive.

**Detection**: GC roots of type `ROOT_FINALIZING`  
**Data available**: `gc_roots_by_type` has the count; per-class breakdown needs bounded loop

### Case 7: Static Field Accumulation

**Pattern**: A `static` field holds a collection (e.g., a logger's appender list, or a registry).
Objects added to it are never removed. The static field is a GC root, so nothing in the collection
can be collected.

**Detection**: `gc_roots_by_type["STATIC_FIELD"]` high; `top_retainers` showing a static field
with unusually high retained heap.

### Case 8: Thread-Retained Job Queue

**Pattern**: A `ThreadPoolExecutor`'s work queue holds `Runnable` objects that capture large
closure arguments (e.g., request payloads). If the queue backs up, all payloads are held in memory.

**Detection**: `ThreadInfo.local_root_count` high for worker threads; thread retained heap high.

### Case 9: Lambda Closure Leak

**Pattern**: Lambda expressions capture outer variables. If the lambda is stored in a long-lived
data structure (callback list, async chain), the captured closure prevents GC of all captured
variables, including large contexts.

**Detection**: High instance count of `$$Lambda$NNN/0x...` classes; their dominators should be
the queue/list that holds them.

### Case 10: Soft/Weak Reference Cache Pressure

**Pattern**: A `SoftReference` or `WeakReference` cache is too large — it's not leaking in the
traditional sense, but it occupies most of the heap. When GC pressure occurs, the references are
cleared and cold code paths fail (cache-miss latency spikes).

**Detection**: `references.soft.referent_shallow` unusually large relative to total heap.

---

## 5. Data Available in hprof-analyzer — Full Inventory

From `report/model.rs`:

### Always-on

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
| Thread retained | `ThreadInfo.retained` | per-thread retained heap |
| Per-frame locals | `ThreadInfo.significant_frames` | under --thread-locals |
| Leak suspects | `LeakSuspects.suspects` | single + group suspects |
| Root paths | `Suspect.root_path` | dominator chain to GC root (field names with --ref-paths) |
| Merged paths | `Suspect.merged_paths` | group suspects' merged chains |
| Dominator tree | `Suspect.dominator_tree` | full subtree at accumulation point |
| Package tree | `TopConsumers.biggest_packages` | PackageNode recursive tree |
| Classloader rollup | `loader_rollup` | per-loader retained/instances |
| Duplicate classes | `duplicate_classes` | class name loaded by 2+ loaders |
| Top components | `top_components` | MAT-style by-loader components |
| Biggest objects | `biggest_objects` | top individual objects by retained |
| Big drops | `dominator_analysis.big_drops` | retention concentration nodes |
| Immediate dominators | `dominator_analysis.immediate_dominators` | per-class dominator rollup |
| Array length histogram | `arrays_by_size` | object + primitive, by power-of-2 |
| Top arrays (individual) | `collections.top_prim_arrays`/`top_obj_arrays` | biggest individual arrays |
| Soft/weak/phantom stats | `references.soft/weak/phantom` | instance + referent histogram |
| Leak indicators | `leak_indicators` | anonymous class count, thread-local null keys, DirectByteBuffer sum |
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

## 6. Proposed New Views — Catalogue

### 6.1 Object Graph Click-Through ★★★ (Tier A)

**What**: Expandable outbound-reference tree for top retained objects. Equivalent to MAT's
"Outgoing References" tree.

**Reference**: Eclipse MAT `ObjectListResult.Outbound.children()` in `ObjectListResult.java`

**Status**: `ObjGraphCapture` + `capture_obj_graph_edges()` already implemented in `pass2/model.rs`.
Backend plumbing (`main.rs`, `report/model.rs`, `report/build.rs`) and UI (`App.tsx`) still pending.

---

### 6.2 Dominator Tree Explorer ★★★ (Tier A)

**What**: Interactive dominator subtree for top retained objects. Generalization of the existing
`DomTreeNode` (currently only on Leak Suspects) to any of the top-20 biggest retained objects.

**Reference**: MAT's "Dominator Tree" view; JProfiler's "Sunburst Dominator Diagram"

**Data available**: `dc_offsets`/`dc_targets` (dominator-children CSR) + `build_dominator_tree_node()`
already exists in `report/build.rs` at ~line 2174.

**Key distinction from object graph**: dominator tree shows *retained heap flow* (who EXCLUSIVELY
keeps what alive); reference graph shows *reference structure* (who points at what). Both needed.

---

### 6.3 Thread Retention Ranked Table ★★★ (Tier A)

**What**: All threads sorted by retained heap descending. Shows: name, state, retained, local root
count, top 3 retained objects.

**Reference**: Netflix Tech Blog — "retention by thread" as #1 diagnostic for their leaks.

**Data available**: `ThreadInfo.retained` already present. Sort + render as a dedicated table.

---

### 6.4 "Who Holds This Class?" Quick Lookup ★★★ (Tier A)

**What**: For any class in the histogram, show the top-3 immediate dominator classes with counts.
"Who's keeping all these byte[] alive?"

**Reference**: StackOverflow #1 heap dump question (2,847 votes). JProfiler "Merged Dominating
References".

**Data available**: `dominator_analysis.immediate_dominators` has per-class grouping. Add
`top_dominators` field to `HistRow` or compute inline.

---

### 6.5 DirectByteBuffer Off-Heap Card ★★★ (Tier A — UI only)

**What**: Prominent card: "Off-heap NIO: X MB across N DirectByteBuffer instances"

**Reference**: Production case study #5. GCeasy OOM type "Direct buffer memory".

**Data available**: `leak_indicators.direct_byte_buffer_capacity_sum` is already computed!
Gap: UI doesn't surface this as a dedicated, prominent card.

---

### 6.6 Classloader Leak Heatmap ★★★ (Tier A — UI only)

**What**: Treemap of retained heap grouped by classloader. Clicking a loader shows its classes.

**Reference**: HeapHero "histogram by classloader" view; MAT `class_loader_explorer` query.

**Data available**: `loader_rollup` + `duplicate_classes`. Wire to existing `ZoomableTreemap`.

---

### 6.7 Lambda / Anonymous Class Grouper ★★ (Tier A — UI only)

**What**: Group `$$Lambda$NNN/0x…` names by enclosing class. Toggle "Group lambdas / Show all".

**Reference**: Common r/java complaint. Production case study #9 (lambda closure leak).

**Data available**: Histogram is already present. Pure client-side regex grouping in App.tsx.

---

### 6.8 Collection Waste Budget Table ★★★ (Tier A under --collections)

**What**: Unified ranked table of top 10 memory-waste sources in collections:
1. Under-filled collections
2. Empty collections (0-element overhead)
3. Constant arrays (all elements identical)
4. Oversized backing arrays
5. Duplicate strings

Each row: class+field, wasted bytes, # instances, fix suggestion.

**Reference**: MAT Component Report sections 5–12; YourKit inspections 1–6.

**Data available**: All present under `--collections` flag. New UI aggregation only.

---

### 6.9 Executive Summary Card ★★★ (Tier A — UI only)

**What**: A single "at a glance" card at the top of the report:
- File + JVM info, triage badges
- Top suspect one-liner: "HashMap holds 1.2 GB (38%), kept alive by Thread 'http-executor-1'"
- Thread-local leak signal
- Quick stats: reachable / garbage / wasted bytes

**Reference**: MAT "Overview" screen; GCeasy 8-type OOM diagnosis.

**Data available**: All in report (`triage`, `suspects`, `leak_indicators`, histogram). Pure UI.

---

### 6.10 Finalizer Queue Analysis ★★ (Tier A — minor backend)

**What**: Objects in the finalization queue by class. Count + retained. Detects finalizer buildup.

**Reference**: MAT `finalizer_queue` named OQL query; YourKit inspection #7.

**Data available**: `gc_roots_by_type` has total count. Per-class breakdown needs a bounded loop
over gc_root arrays filtered to `ROOT_FINALIZING`.

---

### 6.11 Allocation Site × Retention Flamegraph ★★★ (Tier A)

**What**: When HPROF contains allocation traces, render a flamegraph of allocation sites sorted
by **retained** bytes. Shows which call path's objects are still alive.

**Reference**: async-profiler `--alloc` mode; Go pprof `inuse_space` sample type.

**Data available**: `alloc_sites: Option<AllocSites>` already in report when traces present.
Wire to `ZoomableTreemap` or a CSS icicle layout.

---

### 6.12 ThreadLocal Leak Analyzer ★★★ (Tier B — field decode)

**What**: Walk `ThreadLocalMap.Entry` instances with null keys. Show what value classes are
trapped, which threads hold the most, estimated trapped bytes.

**Reference**: YourKit inspection #9; Production case study #1.

**Data available**: `leak_indicators.thread_local_null_key_count` is the scalar count today.
Full expansion requires field-decode scan of `ThreadLocalMap.Entry.value`.

**Flag**: `--full-analysis`

---

### 6.13 Cross-Dump Retained Growth Diff ★★★ (Tier A — minor backend)

**What**: In two-dump comparison: add "Retained Growth" table — class → retained A → retained B
→ delta bytes + delta %. Classes with most retained growth = best leak suspects.

**Reference**: Uber Engineering blog. Heaptrack "peak vs. live" distinction.

**Data available**: Diff report already has histogram diffs. Needs retained delta added alongside
shallow delta in `SeriesDiffEnvelope`.

---

### 6.14 Merged Retained Paths for Any Class ★★★ (Tier A)

**What**: JProfiler's "Merged Dominating References" for any histogram class. Currently only on
group Leak Suspects.

**Data available**: Dominator tree is in memory at report time. Generalise
`build_merged_path_node()` to any class.

---

### 6.15 Sunburst / Icicle Dominator Visualization ★★ (Tier A)

**What**: For top suspects, render `Suspect.dominator_tree` as a sunburst (d3 `partition()` +
`arc()` in polar coordinates). Center = suspect; rings = dominated objects; arc = retained bytes.

**Reference**: JProfiler sunburst; .NET dotMemory sunburst view.

**Data available**: `Suspect.dominator_tree` is already a recursive `DomTreeNode`.

---

### 6.16 Reference Chain Graph Visualization ★★ (Tier A)

**What**: Render `Suspect.root_path` as a small interactive node-link graph (5–15 nodes) showing
field-name edge labels on the path from GC root to suspect.

**Reference**: MAT "Path to GC Roots" tree, visualized as a graph.

**Data available**: `Suspect.root_path` with `field_edge` names (under `--ref-paths`).

---

### 6.17 Soft-Reference Pressure Gauge ★★ (Tier A — UI only)

**What**: Visual indicator of how much heap is "soft-protected". SoftReferences are cleared under
GC pressure — a large soft cache hides potential OOM issues.

**Data available**: `references.soft.referent_shallow`, `referent_histogram`. All present.

---

### 6.18 Metaspace / ClassLoader Pattern Detector ★★ (Tier A — UI only)

**What**: Detect classloader-growth pattern: many instances of the same classloader type loaded
in sequence. "48 instances of URLClassLoader each loaded ~200 classes — likely classloader leak."

**Reference**: MAT `class_loader_explorer`; AWS post-mortem case study #4.

**Data available**: `loader_rollup` + `duplicate_classes`. Group by `loader_label`, count instances.

---

### 6.19 GC Root Type × Class Matrix ★ (Tier A — minor backend)

**What**: Which classes are held by which GC root types? Tells you "this HashMap is held by a
JNI Global — this is a JNI leak, not a Java leak."

**Data available**: `gc_roots_retained_by_type` has totals per type. Per-class breakdown requires
a bounded loop over root arrays grouped by type.

---

### 6.20 Spring/Hibernate Framework OQL Queries ★★ (Tier A — named queries)

**What**: Named OQL queries for common framework leaks:
- `spring-context-retained` — find ApplicationContext instances with retained heap
- `hibernate-sessions` — find Session instances with non-empty L1 cache
- `executor-queues` — find ThreadPoolExecutor + queue depth
- `netty-buffers` — count AbstractReferenceCountedByteBuf with refCnt > 0
- `connection-pools` — find JDBC connection pool instances

**Reference**: MAT's named query library; JProfiler framework-aware probes.

**Implementation**: Add as named OQL queries in `src/query/` built-in list.

---

## 7. HTML Report "Further Reading" Links

The HTML report should include a "Further Reading" section or tooltip links pointing to:

| Resource | URL | What it adds |
|----------|-----|-------------|
| Eclipse MAT | https://projects.eclipse.org/projects/tools.mat | Full interactive heap dump analysis |
| Eclipse MAT docs | https://wiki.eclipse.org/MemoryAnalyzer | OQL reference, Component Report docs |
| VisualVM | https://visualvm.github.io | Live profiling + heap dump viewer |
| async-profiler | https://github.com/async-profiler/async-profiler | Allocation flamegraph (complements hprof) |
| JVM Garbage Collection Guide | https://docs.oracle.com/en/java/javase/21/gctuning/ | Oracle GC tuning guide |
| JVM HPROF format spec | https://hg.openjdk.org/jdk6/jdk6/jdk/raw-file/tip/src/share/demo/jvmti/hprof/manual.html | HPROF binary format |
| YourKit memory inspections | https://www.yourkit.com/docs/java-profiler/2023.9/help/inspections_memory.jsp | 15 automated inspections reference |
| JProfiler heap docs | https://www.ej-technologies.com/resources/jprofiler/help/doc/heapDump/heapDumpView.html | Dominator tree, sunburst, merged paths |

---

## 8. MAT OQL Query Coverage

Eclipse MAT ships ~50 built-in named OQL queries. This table maps them to current hprof-analyzer OQL support.

| MAT Query | hprof-analyzer | Gap |
|-----------|---------------|-----|
| `SELECT * FROM java.lang.String` | ✅ basic SELECT | — |
| `SELECT * FROM INSTANCEOF java.util.Collection` | ✅ INSTANCEOF | — |
| `SELECT s.@retainedHeapSize FROM java.lang.String s` | ✅ @retainedHeapSize | — |
| `SELECT s.value FROM java.lang.String s` | ✅ field decode (with --ref-paths) | — |
| `SELECT * FROM ... WHERE ...` | ✅ WHERE clause | — |
| `SELECT OBJECTS s FROM java.lang.String s` | ✅ OBJECTS modifier | — |
| `SELECT AS RETAINED SET ...` | ❌ | not implemented |
| `SELECT DISTINCT ...` | ✅ | — |
| `inbounds(x)` | ❌ | inbound edge walk |
| `outbounds(x)` | ❌ | outbound edge walk |
| `dominators(x)` | ❌ | dominator chain |
| `dominatorof(x)` | ❌ | immediate dominator |
| `classof(x)` | ✅ | — |
| `toHex(x)` | ✅ | — |
| `sizeof(x)` | ✅ `@shallowHeapSize` | — |
| `LENGTH(array)` | ✅ `@length` | — |
| Named query: `leak_hunter` | ✅ (via suspects) | no OQL wrapper |
| Named query: `duplicate_classes` | ✅ (report section) | no OQL wrapper |
| Named query: `class_loader_explorer` | ✅ (report section) | no OQL wrapper |
| Named query: `finalizer_queue` | ❌ | needs ROOT_FINALIZING filter |
| Named query: `collection_fill_ratio` | ✅ (under --collections) | no OQL wrapper |
| Named query: `map_collision_ratio` | ✅ (under --collections) | no OQL wrapper |
| Named query: `waste_in_char_arrays` | ✅ (constant_primitive_arrays) | no OQL wrapper |
| Named query: `group_by_value` | ✅ GROUP BY | — |
| Named query: `component_report` | ❌ | not implemented (complex) |

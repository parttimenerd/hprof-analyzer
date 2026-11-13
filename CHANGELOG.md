# Changelog

All notable changes to hprof-analyzer are documented here.

## [Unreleased]

### Added

- **MCP `get_gc_roots` tool** — lists all GC roots in the heap (objects whose
  immediate dominator is the virtual root). Returns index, class, shallow, and
  retained size per root, with an optional `limit` parameter. Available after
  any `load_dump` (no `with_graph` needed).

- **MCP `get_field_name` tool** — look up the Java field name for a specific
  reference edge by source object index and edge index within that object's
  forward-edge list. Requires `load_dump({with_graph: true})`. The field name
  pool is persisted in the cache (`field_name_pool.json.zst`) so it is
  available on repeat loads without re-analyzing the dump.

- **`heap field-name` CLI subcommand** — CLI counterpart of `get_field_name`:
  `heap field-name <dump> --src N --edge N`. Requires graph cache (`heap load
  --with-graph`).

- **Report: `pct_of_retained` on `Component`** — each component in the
  Components table now includes `pct_of_retained`: what percentage its retained
  heap is of the total retained heap (complements the existing `pct` which is
  percentage of shallow heap). Shown as a `% Retained` column in Markdown
  output.

### Fixed

- **Backward compatibility for `pct_of_retained`** — reports generated before
  this field was added (`#[serde(default)]`) now deserialize cleanly; old JSON
  files no longer cause a missing-field error.

### Tests

- **Parity test trimmed to two fixtures** — `parity_all_fixtures` previously
  ran against all 5 present dumps (10 binary invocations). Now runs only
  `dump_4_philosophers` and `dump_2_scala-doku`, which cover the two distinct
  class-loader layouts. Both `REGEN_FIXTURES=1` and `HPROF_UPDATE_SNAPSHOTS=1`
  trigger baseline regeneration.

- **MCP: three new graph-primitive tools for LLM-driven heap exploration** —
  these expose the cached dense arrays directly, enabling iterative graph
  traversal without OQL round-trips:

  - **`get_dominator_path`** — walk the immediate-dominator chain (`idom[]`)
    from any object up to the GC virtual root. Returns the full retention chain
    with class name, shallow, and retained sizes per node. Available after any
    `load_dump` (no `with_graph` needed). Use to answer "why is this object
    retained?"

  - **`get_outbound_references`** — list all objects a given object directly
    references (forward CSR edges). Returns target index, class, shallow, and
    retained for each reference. Requires `load_dump({with_graph: true})`.
    Complements `find_incoming_references` (backward direction).

  - **`get_retained_set`** — enumerate the dominator subtree of an object: all
    objects that would be freed if this one were collected. Priority-queue
    traversal returns the largest-retained objects first regardless of depth.
    Includes a `class_summary` (top 20 classes by total retained in the
    subtree). Optional `class_filter` (substring) focuses on specific classes.
    Safety caps: 100 K nodes visited, depth ≤ 100.

  Together with the existing `browse_dominators` (downward tree walk) and
  `find_incoming_references` (backward edges), these give an LLM complete
  graph traversal in all five directions: upward, downward, forward, backward,
  and subtree enumeration.

- **MCP `find_incoming_references` now uses the full inbound CSR** — previously
  read from `obj_graph_flat.inbound_edges`, a significance-filtered viz graph
  that silently omits objects below the retained-heap significance floor. The
  tool now reads the full `inb_off`/`inb_tgt` CSR (same source as
  `inspect_object`), so every referencing object is visible. Requires
  `with_graph=true` on `load_dump`; returns a clear error otherwise. `field_name`
  is removed from the output (not available in the CSR — use `inspect_object` for
  field-level detail).

- **`heap inspect` shows inbound references when graph cache is present** — the
  CLI now auto-detects whether the graph cache exists and, if so, loads
  `CacheMode::Graph` and appends an "Referenced by" section (up to 20 referrers
  with class, shallow, and retained sizes) to both text and `--json` output. When
  the graph cache is absent, a hint to re-run with `heap load --with-graph` is
  shown instead.

- **New `heap find-incoming` CLI subcommand** — lists all objects that directly
  reference a given object: `heap find-incoming <dump> --index N [--limit N]
  [--json]`. Reads from the full inbound CSR (requires graph cache). Fills the
  gap left by having `browse_dominators` (down) but no CLI equivalent of MCP
  `find_incoming_references` (up).

- **OQL: class names populated for `ObjRef` in dominator/refpath/list/map
  expansion** — `dominator_rows`, `refpath_rows`, `expand_list_array`, and
  `expand_map_table` all hardcoded `"?"` as the class string; now resolved via
  `ctx.class_name_of()`.

- **OQL: VALUES-only queries short-circuited in `run_oql_query`** — inline
  `VALUES (…)` tables (e.g. `SELECT * FROM VALUES (1,'a') AS t(id,s)`) no
  longer trigger a heap scan when executed through the MCP `query` tool or
  `heap query` CLI. The escalated path now calls
  `run_values_only_shortcircuit()` before the scan, matching the behaviour of
  `run_single_dump`.

- **P1 — Pool state snapshot in Leak Indicators** — scan-time detection of common
  Java thread/connection pool instances (HikariCP, Tomcat JDBC, DBCP2, C3P0,
  Spring `ThreadPoolTaskExecutor`, `ForkJoinPool`, `ExecutorService`). Each pool
  type renders a table showing class name, instance count, total retained, and
  per-instance pool-size / max-pool-size / queue-depth / active-count columns.
  Only emitted when at least one recognized pool class is present.

- **P2 — Finalizer referents by class** — `java.lang.ref.Finalizer.referent`
  is scanned at pass2 field-decode time and per-instance `(finalizer, referent)`
  pairs are stored in `Graph.finalizer_instances`. `build_finalizer_classes()`
  groups them by referent class name and sums retained bytes via `g.retained[]`.
  The result renders as a "Finalizer referents" table in Leak Indicators:
  referent class / count / retained. Cache VERSION → 15, SCHEMA_VERSION → 16.

- **B4 — DirectByteBuffer per-buffer owner table** — when `dbb_top_records` is
  populated (requires `--full-analysis`), a "Top DirectByteBuffer owners" table
  now appears in the Leak Indicators section showing each buffer's capacity, its
  retained size, and its dominator root-path chain.

- **OQL: `MEDIAN` / `PERCENTILE(n)` in `VALUES` queries and retained `GROUP BY`**
  — `SELECT MEDIAN(x.@retainedHeapSize) FROM java.lang.String x` and
  `SELECT PERCENTILE(90, x.@retainedHeapSize) FROM … GROUP BY …` now work
  correctly. Even-*n* `MEDIAN` averages the two middle values; `PERCENTILE`
  uses the nearest-rank formula. Previously fell through to `Null`.

- **OQL: array identity method dispatch** — `getObjectAddress()`,
  `getObjectId()`, `getUsedHeapSize()`, and `getRetainedHeapSize()` now work
  on array instances in the method-dispatch path (`execute.rs`), returning the
  same values as the equivalent `@` virtual attributes.

- **OQL: `VALUES (…)` inline table source**  — embed literal rows directly in a
  query: `SELECT * FROM VALUES (1, 'a'), (2, 'b') AS t(n, s)`. Column names
  from the `AS t(…)` alias are available as `@col0`, `@col1`, … or by the
  declared names. Supports `WHERE`, aggregates, `ORDER BY`, `LIMIT`, and
  `JOIN`.

- **OQL: `INNER JOIN` / `LEFT JOIN`** — join heap query results against
  `VALUES` tables or one of three built-in virtual tables:
  - `classes` — `(class_name, instance_count, shallow_total, retained_total)`
  - `gc_roots` — `(dense_idx, root_type, root_type_name)`
  - `threads` — `(serial, obj_idx, name, is_daemon, priority)`

  Bare `JOIN` is treated as `INNER JOIN`. Example: `SELECT x.@className,
  c.retained_total FROM java.lang.Object x JOIN classes c ON x.@className =
  c.class_name`.

- **OQL: `NOT IN (subquery)`** — `@objectAddress NOT IN (SELECT
  @objectAddress FROM SomeClass)` excludes objects whose address appears in
  the inner result. Desugars to `NOT (addr IN (…))` so no executor changes
  were needed.

- **OQL: `GROUP BY` over `RefPath` hops and `@valueArray`** — group-by
  expressions may now traverse reference paths (e.g. `GROUP BY x.field.key`)
  and use `@valueArray` as a grouping key. Previously only `SELECT` and
  `WHERE` expressions were lowered to RefPath carry mode; the planner now
  propagates `group_by_hops` into the carry plan and the stage runner
  evaluates multi-hop GROUP BY keys in `join_retained_group_by`.

- **OQL: `FROM ENTRIES <ClassName>` — full collection expansion** — expanded
  to support `HashMap`, `LinkedHashMap`, and `HashSet` in addition to
  `ArrayList`/`Vector`/`ArrayDeque`. Map entries yield `(key, value)` rows;
  list entries yield `(index, element)` rows. Unknown collection types return
  an informative note instead of silently producing no rows.

- **OQL: `@fillRatio` virtual attribute** — returns the backing-array fill ratio
  (`non_null_slots / backing_array_length`) for known collection types (e.g.
  `HashMap`, `ArrayList`) as a float 0.0–1.0. Returns `Null` for objects that are
  not recognized collections or lack a backing array field. Requires the forward
  reference graph (`--full-analysis` or when running in the late phase), which is
  activated automatically when `@fillRatio` appears in a query.

- **OQL: new virtual attributes** — `@category` (classifier bucket: `"jdk"` /
  `"framework"` / `"other"` / user-defined), `@superclass` (immediate superclass
  name), `@collectionSize` (logical element count from the collection's size
  field), `@dominatorClass`¹ (class name of the immediate dominator), and
  `@dominatorDepth`¹ (distance in dominator hops from GC roots).

- **OQL: `pathToRoot(alias)`¹** — walks `idom[]` upward from each matched object
  to the GC root, emitting one row per hop. Mirrors `dominators(alias)` but
  traverses up instead of down.

- **OQL: `FROM ENTRIES <ClassName>`¹** — iterates the logical entries of a Java
  collection. v1 supports `ArrayList`/`Vector` (yields `(index, element)` rows).
  Backed by the shared `CollDesc` registry in `src/collections.rs`.

- **OQL: SQL LIKE wildcards** — `LIKE "%foo%"` and `LIKE ".*foo.*"` are now
  equivalent. `%` auto-translates to `.*` and `_` to `.` before regex
  compilation, so both SQL-glob and regex styles work interchangeably.

- **Shared collection registry (`src/collections.rs`)** — `CollDesc`,
  `CollKind`, and `builtin_coll_descs()` moved from `pass2/fielddecode.rs` into
  a new shared module, making collection metadata available to both the pass2
  fill-ratio analysis and the OQL engine. User-defined entries via
  `[[collection]]` in `.hprof-analyzer.toml` are prepended ahead of built-ins.

- **`class_super_idx` on `Graph`** — O(1) superclass lookup for OQL. Populated
  during the pass1→pass2 bridge from `ClassInfo.super_id`. Cache VERSION bumped
  to 14.

- **⛓ Retention Chains tab** — a new tab in the Top Consumers section shows
  class-level aggregated retention chains for classes where the dominator Sankey
  misses the full picture (retained heap split across millions of per-instance
  pairs). Each card shows the upstream holder chain (e.g.
  `HashMap → HashMap$Node[] → HashMap$Node`) with instance counts, retained
  bytes, and a proportional retention bar. The tab only appears when multi-hop
  holders are present.

- **Multi-hop upstream holders in Biggest Classes** — the holder breakdown now
  surfaces upstream classes up to 3 hops away (previously 2). A third O(n) pass
  (`level3_accum`) over level-2 holder classes enables 3-hop bubble-up; the
  `via_class` field on `HolderRow` is now `Vec<String>` (the full intermediate
  path) instead of `Option<String>`. Cache VERSION bumped to 11.

- **`category` field on `ClassRow` and `UnreachableClassRow`** — every class in
  the Biggest Classes list and Unreachable Objects section now carries a
  `category` string (`"jdk"`, `"framework"`, `"other"`, or any user-defined name)
  populated by the classifier at report-build time. The UI uses this field for
  accurate filtering; older reports without the field fall back to a regex
  heuristic for backward compatibility.

- **Dynamic category filter buttons** — the category filter toolbar is now fully
  data-driven: it shows exactly the categories present in the loaded report, user-
  defined category names (from `--category`/`--category-config`) appear as
  additional buttons alongside the built-in Other / Framework / JDK. The bar hides
  itself when all classes belong to a single category (no filter would change
  anything).

- **Category filter on System Overview histogram and Unreachable Objects** — the
  dynamic filter bar now appears on the System Overview class histogram and the
  Unreachable Objects section in addition to the existing Top Consumers coverage,
  so all class-level views support category filtering.

- **`heap` subcommand `--category` / `--category-config` support** — the cached
  analysis (`heap report`, `heap query`, `heap summary`, `heap histogram`) now
  accepts the same `--category NAME=PREFIX` and `--category-config PATH` flags as
  the top-level command, so cache-rebuild runs use the user's custom categories.

- **`--obj-graph` hot-path speedup** — the `capture_type_ref_and_obj_graph`
  inner loop (O(n×edges)) now uses a packed `u64` key and a Fibonacci-multiply
  integer hasher instead of SipHash on `(u32,u32)` tuples. Expected measurable
  reduction in wall time for large dumps when `--obj-graph` is active.

- **Drop 2+ `.hprof` files to compare** — the browser app now accepts multiple
  heap dump files at once (via drag-and-drop or file picker); it analyzes each
  sequentially via WASM and renders the full diff automatically, no CLI step
  needed. Per-file status rows show parse / load / analyze / report progress.

- **Richer diff view** — the multi-dump comparison now includes:
  - **Δ instances column** in the Growth Leaders, New Classes, Removed Classes,
    Leak Suspects, and Spike tables — instance-count delta alongside retained-heap
    delta, so both allocation growth and retention growth are visible at once.
  - **Growth Leaders — Heap Footprint** section — side-by-side `FlatTreemap`
    tiles comparing the baseline (r1) vs. final (rN) retained-heap distribution
    of the top growth classes, giving an immediate visual of where memory moved.

- **🔥 Flame view in Dominator Analysis** — a fourth view toggle ("🔥 Flame")
  in the Dominator Analysis section renders the immediate-dominator class data
  as an interactive `ZoomableTreemap` (treemap + flamegraph). Tile area equals
  dominated shallow heap; clicking zooms into a class subtree.

### Performance

- **~5 GB RSS reduction in `heap report` / MCP `load_dump` slow path** — two
  fixes combined bring `heap report` peak RSS from ~24.6 GB to ~19.5 GB on the
  34 GB reference dump:

  1. The `analyze_with_cache` slow path previously captured `hprof_offsets` (a
     2.56 GB `Vec<u64>`) during the main pass1 run AND immediately cloned a
     `Pass1Snapshot` (1.28 GB `IdMap` offsets), leaving all three alive
     simultaneously. Fix: pass1 runs with `capture_hprof_offsets = false` for
     the main analysis; a separate short pass1 run writes `offsets.bin` after
     analysis completes. Net cost: ~20–40 s extra wall time for the offsets pass.

  2. `analyze_to_report_inner_with_cache` previously held the forward CSR
     (~7–8 GB) through `compress_parent_pre` and then called `build_from_fwd`
     unconditionally. Fix: when `total_inb > 1B` and object-graph/field-stats
     modes are inactive, the fwd CSR is dropped before `compress_parent_pre`
     and `build_mat_scan` rescans the file for inbound edges instead.

  Measured on 34 GB reference dump (commit b5306da): 24.59 GB → **19.52 GB**
  peak RSS, wall time 16:15.

- **~0.5 GB RSS reduction in direct-path basic analysis on large dumps** — the
  forward CSR (`fwd_targets` + `fwd_offsets`, ~7–8 GB on a 34 GB dump) was held
  alive through `compress_parent_pre` even though the rescan-inbound path drops it
  immediately at the inbound dispatch that follows. The overlap created a peak where
  fwd CSR + `parent_pre` (~1.3 GB) + `rpo.dfn` (~1.3 GB) all coexisted.

  Fix: when all of `--mat-caches`, `--obj-graph`, `--field-stats`, and the
  retain-forward/inbound flags are inactive AND `total_inb > 1B` (confirms the
  rescan path), the fwd CSR is dropped right after `unreachable_retained` — before
  `compress_parent_pre`. Measured on 34 GB reference dump: 14.57 GB → **14.12 GB**
  peak RSS, wall time 14:08.

### Fixed

- **OQL: `SELECT *` in grouped queries is now an error** — previously accepted
  silently and produced undefined column output. Now returns a clear message:
  _"SELECT * is not supported in a grouped query; list columns explicitly"_.

- **OQL: `MIN` / `MAX` use `total_cmp` for NaN consistency** — NaN now
  consistently sorts last regardless of row order, matching the SQL expectation
  that `MIN`/`MAX` over an all-NaN group returns NaN as the sole survivor.

- **OQL: `VALUES` queries apply `OFFSET` before `LIMIT`** — previously `LIMIT`
  was applied to the pre-offset rows (effectively ignoring `OFFSET`). Now rows
  are drained by `OFFSET` first, then truncated by `LIMIT`.

- **Dead `⬡` and `⬡≡` buttons in diff/compare tables** — the pivot-to-dominator
  (`⬡`) and list-instances (`⬡≡`) buttons appeared in diff table class-name
  cells but did nothing: `PivotBtn` renders null when `HasDomDataCtx` is false
  (always the case in the diff view, which has no loaded WASM session), and
  `ListObjectsBtn` fires a `CustomEvent("inspect")` that no listener handles.
  Both are now removed from `SeriesTable`, `SpikeTable`, and `TpfgDiffTable`.
  The copy (⎘) and OQL (⌗) buttons remain — they work everywhere.


  single-dump HTML report lets users drag-and-drop two or more report JSON files
  and see the full diff rendered instantly, no CLI required. The diff covers
  Growth Leaders, New/Removed Classes, Transient Spikes, Suspect changes,
  TPFG changes, Triage Signal changes (new/resolved rules between dumps), and
  Leak Indicator deltas (DirectByteBuffer, anon classes, ThreadLocal null keys).

- **`dominatorof(x)` OQL function** — `SELECT dominatorof(x) FROM java.lang.String x`
  returns the immediate dominator of each object. The planner sets
  `needs.dominator_children = true` which routes the query through the full
  retained+dominator pipeline, so `idom[]` is available in `LateCtx`. Usable
  in any `SELECT` projection or `WHERE` predicate; works in the REPL, MCP
  `query` tool, and `heap query` CLI.

- **`compare_dumps` MCP tool** — compare two heap dump reports side by side
  directly from an MCP session: returns growth leaders (by Δ retained),
  new classes, and grown suspects with delta figures. Complements the existing
  `compare reports --format html` CLI.

- **Soft-reference retained total in References section** — both the HTML report
  and Markdown/CLI output now show "**X MB** only softly retained — would be
  freed under OOM pressure" for `Soft` reference groups, making it immediately
  clear how much memory the GC could reclaim if it needed to.

- **ThreadLocal pool spread surfaced in Threads section** — the HTML report and
  Markdown output surface the `ThreadLocalPoolAccumulation` signal data:
  per-pool-thread retained heap and the "spread" count (how many threads each
  carry a copy of the same leaked value).

- **Collection Retained Heap and Resource Handles sections in HTML report** —
  the HTML report now renders the `static_collections` and `resources` sections
  from the report model, surfacing static-field-anchored collection sizes and
  unclosed resource handles (zip streams, file channels, etc.) alongside the
  existing collection waste analysis.

- **`find_incoming_references_by_class` MCP tool** — given a fully-qualified
  class name, returns all inbound references to instances of that class grouped
  by source class, with aggregate counts and retained-heap totals. The
  histogram-investigation workflow MAT users reach for constantly ("what holds
  all these `String` instances?"), now available in a single MCP call.

- **6 new `get_report` MCP sections** — `gc-roots`, `classloaders`, `strings`,
  `packages`, `culprits`, `self-referential` — expose existing `Report` data
  under friendlier section names without requiring the caller to parse the
  giant `overview` or `top` blob.

- **6 exploration OQL named views** — `xml-dom-documents`, `fjp-task-queue`,
  `classloader-instances`, `big-decimal-instances`, `http-sessions`,
  `thread-pool-waiters` — surfaced in `list_views` output and cross-linked from
  relevant triage signal hints.

- **Off-Heap NIO panel redesign** (HTML report) — 8-bucket capacity histogram
  (from `<1 KB` to `≥16 MB`) with a stacked distribution bar, fragmentation
  risk card, and a "Top Buffer Owners" table showing the largest
  `DirectByteBuffer` instances with their dominator root-path chain.
  `LeakIndicators.nio_buffer_owners` carries the new owner records.
  Schema v15; `cache::VERSION` bumped.

- **Thread state overview and pool-group analysis** (HTML report) — the Threads
  section now shows a thread-state pie (RUNNABLE / WAITING / TIMED_WAITING /
  BLOCKED / NEW / TERMINATED) and groups threads by pool prefix (e.g.
  `ForkJoinPool.commonPool-worker-*`) with per-group aggregate retained heap.

- **Dominant retained class on `ThreadInfo`** — `thread_info.dominant_retained_class`
  names the single class responsible for the most retained bytes within each
  thread's retention subgraph.

- **Triage follow-up hints** — `ThreadLocalSpreadLeak`, `UnclosedResourceLeak`,
  and `ConnectionLeak` signal texts now include concrete next-step OQL snippets
  pointing at the named investigation views.

- **Track 2 triage rules**: `XmlDomNodeAccumulation`, `ForkJoinBlobAccumulation`,
  `ClassLoaderProliferation`, `HttpSessionAccumulation`, `BigNumberAccumulation`,
  `ConnectionPoolExhaustion`, plus threshold fixes for `ParserOutputAccumulation`
  (floor 32 MB → 8 MB), `ScheduledTaskAccumulation` (also matches
  `ForkJoinTask`), `ClassloaderLeak` (Warning fires at ≥ 5 loaders, not ≥ 10
  duplicate classes), and `SessionScopeLeak` (retained-bytes path for "few fat
  sessions").

- **5 additional triage rules** from the victorrentea gap-coverage analysis:
  `ShutdownHookThreadCount`, `StaticCacheGrowth`, `ReflectionProxyLeaks`,
  `SoftReferenceOverRetention`, `WeakHashMapValueLeak`.

- **JDK/App/All filter toggle** on the Biggest Classes table in the HTML report
  — filter the histogram to application-owned classes only, isolating user-code
  retention from JDK/framework noise.

- **`Report.waste_breakdown`** — unified "Where Is My Memory Going?" breakdown,
  one row per fired waste-source rule with its `recoverable_bytes`. Exposed
  in `heap report --section waste`, the MCP `waste` section, and the Markdown
  report. Grows automatically with new waste-category rules; no second
  hand-maintained list.

- **`Report.primary_culprits`** — the "who's actually responsible" answer,
  resolved once from `leaks.suspects` and shared by `Headline
  Retainer`/`Concentration`/`Dominant GC-Root Type` instead of each
  independently (and sometimes inconsistently) re-deriving it. Fixes the
  "Thread retains 79%, three redundant headlines" pattern: a nearby GC-root
  holder (e.g. a `Thread` whose stack frame merely transits a deeper
  structure) no longer out-ranks the real, deeper culprit, and the
  redundant `Dominant GC-Root Type` bullet is suppressed once the other two
  already name the same object. Schema v12. — the "who's actually responsible" answer,
  resolved once from `leaks.suspects` and shared by `Headline
  Retainer`/`Concentration`/`Dominant GC-Root Type` instead of each
  independently (and sometimes inconsistently) re-deriving it. Fixes the
  "Thread retains 79%, three redundant headlines" pattern: a nearby GC-root
  holder (e.g. a `Thread` whose stack frame merely transits a deeper
  structure) no longer out-ranks the real, deeper culprit, and the
  redundant `Dominant GC-Root Type` bullet is suppressed once the other two
  already name the same object. Schema v12.

- **`SelfReferentialClassAccumulation` triage rule** — names the specific
  class forming a recursive tree/chain (e.g. a `Node` type whose instances
  nest inside each other) once a meaningful share of its own instances and
  retained bytes are involved; `Report.self_referential_classes` exposes
  the same data directly. Unlike the existing class-agnostic
  `DeepRetentionChain` rule (gated on a 10,000-hop floor tuned for
  pathological cases), this fires on moderate-depth recursive structures
  and names the class. Schema v13.

- **User-extensible class categories** (`src/classify.rs`) — an ordered,
  first-match-wins classifier generalizing "JDK vs. non-JDK" into an
  open-ended registry. Built-ins: `jdk`
  (`java./javax./jdk./sun./com.sun./scala./kotlin.`) and `framework`
  (Hibernate/Spring/Netty/HikariCP/...). Users add their own via
  `.hprof-analyzer.toml`'s new `[[category]]` section or the repeatable
  `--category NAME=PREFIX` CLI flag; both evaluate before the built-ins.
  New "Classes by Category" table in System Overview, a "Biggest Non-JDK
  Classes" view, and a standalone `heap report --section categories` / MCP
  `categories` section. Schema v14.

- **`ThreadLocalPoolAccumulation` triage rule** — detects a `ThreadLocal`
  leak spread evenly across a worker pool (N pooled threads each
  accumulating their own copy of a never-cleared value), a pattern
  `ThreadPinning` structurally cannot catch since it only ever inspects the
  single worst thread.

- **MCP `load_dump` gains `collections`/`find_duplicates`/`full_analysis`/
  `categories` parameters** — these opt-in analysis flags were previously
  CLI-only; an MCP-driven session could never enable collection-waste
  analysis, duplicate detection, or custom categories. Found via an
  MCP/CLI flag-parity audit.

- **Synthetic-HPROF scenario test suite** (`tests/scenarios.rs` +
  `tests/common/heap_builder.rs`) — fast, in-process, no-JVM regression
  tests asserting the report *correctly names the responsible class*
  (via `primary_culprits`/`self_referential_classes`/triage signal ids),
  not just "doesn't crash." Runs under plain `cargo test`; see the file's
  doc comment for how to add a new scenario.

- **`Report.waste_breakdown`** — a unified "Where Is My Memory Going?"
  breakdown derived automatically from the triage `Rule` registry, one row
  per fired waste-source rule with a nonzero `recoverable_bytes` figure.
  Unlike `waste_summary` (a fixed, hand-assembled list), `waste_breakdown`
  grows automatically as new waste-category rules are added; no second
  hand-maintained list to keep in sync. Each row carries a `category`
  (`"Collection Capacity"` / `"Duplication"` / `"Per-Object Overhead"`),
  `rule_id`, `title`, `bytes`, and optional `anchor`. Nine triage rules
  now implement `waste_category`/`recoverable_bytes`:
  `OverCapacityCollections`, `ConstantValueArrays`, `BoxedPrimitiveBloat`,
  `DuplicateStrings`, `CharArraySlack`, `SparseObjectArrays`,
  `FixedPerObjectOverhead`, `DuplicatePrimArrays`, `TinyCollectionOverhead`.
  Schema v15; `cache::VERSION` bumped to 3.

### Fixed

- **OQL: `LIMIT 0` incorrectly set `truncated = true`** — a query like
  `SELECT * FROM Foo LIMIT 0` produced a result with zero rows but
  `truncated: true`, misreporting that rows were capped. Fixed: `LIMIT 0`
  clears `truncated` and does not enter the scan-limit early-stop path.

- **OQL: dot-method syntax on string literals** — `"hello".length()` and
  similar string-method calls on literal values now parse and evaluate
  correctly. Previously the parser required a variable alias on the
  left-hand side; literals were silently rejected.

- **OQL: VALUES executor — WHERE predicates, aggregates, `@rowAddress`** —
  three evaluation bugs in the `VALUES (…)` source:
  - `WHERE` predicates were ignored (all rows passed unconditionally).
  - Aggregate functions (`COUNT(*)`, `SUM(…)`) over a VALUES source produced
    wrong results or panicked.
  - `@rowAddress` projected `Null` instead of the row's virtual address.

- **OQL: window `LIMIT` not applied without `ORDER BY`** — `SELECT
  ROW_NUMBER() … LIMIT 10` with no `ORDER BY` clause skipped the LIMIT cap,
  returning all rows. Fixed by applying LIMIT in the no-ORDER-BY window path.

- **OQL: `PARTITION BY` on an unselected key crashed window evaluation** —
  `PARTITION BY` keys not present in the `SELECT` list caused a bounds
  panic inside `apply_window_functions`. Fixed by passing the hidden
  trailing sort/window key count and names through the window evaluator.

- **OQL: `HAVING` on an unselected `GROUP BY` key returned wrong groups** —
  `GROUP BY a HAVING a > 5 SELECT COUNT(*)` (where `a` is not selected)
  evaluated the HAVING predicate with an empty key slice, letting all groups
  pass. Fixed by retaining the raw key vector alongside finalized rows in
  `join_retained_group_by`.

- **OQL: scan-path aggregate queries ignored `--reachable-only`** —
  queries with `WHERE`/`EXISTS`/expressions that force the `SingleScan` path
  (bypassing the fast `HistogramOnly` path) accumulated results over **all**
  objects, including GC-unreachable ones. `COUNT(*)` and `SUM(@usedHeapSize)`
  therefore disagreed with the histogram-backed equivalents on the same dump.
  Fixed: the scan now captures each visited object's dense index into a
  sidecar; after DFN computation, `recompute_agg_from_reachable` rebuilds the
  aggregate from only the reachable subset (supports `COUNT`, `SUM`, `AVG`,
  `MIN`, `MAX` over `@usedHeapSize` or constant expressions; unsupported
  expressions fall back to the all-objects result unchanged).

- **Gross retained churn displayed raw bytes with a doubled minus sign** —
  `gross_shrink_retained` is already negative in the data model; prepending the
  `−` sign produced `−-35583696 B` instead of `−33.9 MB`. Fixed with
  `Math.abs()` before formatting, in both the verdict sentence and the bullet
  list.

- **`compare_dumps` MCP tool ran both analyses sequentially** — both heap
  analyses now run concurrently via `tokio::try_join!`, cutting wall-clock time
  roughly in half when both dumps need a fresh analysis.

- **HTTP query server returned 400 for internal errors** — serialization
  failures and other server-side errors now return HTTP 500; only
  bad-request errors (invalid OQL, missing parameters) return 400.

- **HTTP query server re-parsed the plan on every cache hit** — `store_plan`
  was called even when the plan was already cached (returned as `prebuilt`),
  causing a redundant parse+plan pass. Fixed: `store_plan` is skipped when
  `prebuilt.is_some()`.

- **`ShutdownHookAccumulation` triage rule fired on every web-server JVM** —
  path 2 (detect `ApplicationShutdownHooks` class present) had no retained-heap
  guard, so any web server process that registers even one shutdown hook
  triggered the rule. Now requires at least 1 MB retained by the thread set
  before firing.

- **OQL: `FROM ENTRIES` failed in the full analysis pipeline** —
  `eliminate_dead_needs` was clearing `plan.needs.ref_walk` for
  `FROM ENTRIES` queries because `EntriesExpand` was not listed as a
  ref-walk consumer. Additionally, `arm_refwalk` did not collect backing-array
  field names from built-in collection descriptors for `FROM ENTRIES` sources.
  Both gaps fixed; `FROM ENTRIES` now works in `heap report` / `load_dump`
  as well as `heap query`.

- **Holder-computation window too narrow** — the `top_n * 2` window used when
  building holder breakdowns in `biggest_classes` was insufficient for classes
  where many instances are held by a single owner; widened to `top_n * 3`.

- **`Concentration` fired redundantly when `HeadlineRetainer` already named the
  same class** — suppressed when the concentration culprit matches the headline
  retainer to eliminate duplicate triage noise.

- **`static-field-anchor` headline didn't name the holding class** — the
  anchoring `Class#field` was present in the supplemental data but not included
  in the rule's headline text; now rendered as "held by `ClassName.fieldName`".

- **`Concentration` fired even without `--collections`** — concentration is
  computed from collection attribution data that requires `--collections`; the
  rule now silently skips when collection data is absent.

- **Back-button navigation broken in single-page app** — hash-based navigation
  pushed duplicate history entries; fixed with `replaceState` for same-hash
  transitions and a `Self-Referential Clusters` sub-section added to the nav.

- **Analysis (Quick) button description was misleading** — the button didn't
  note that collection detection (`--collections`) is disabled in Quick mode;
  now shows an explanatory note.

- **`Node`/`Simple`-style classes invisible in Biggest Classes and "Who
  Holds This Class?"** — six holder/dominator-visibility bugs (BUG-1
  through BUG-6) meant a class whose instances are always held by some
  other object (rather than directly by a GC root) was invisible in
  `biggest_classes` no matter how much heap it collectively retained, and
  had no holder breakdown. Regression-tested in `tests/bug_reproducers.rs`.

- **`jsonify`'s HPCA cache-file version check was hardcoded and drifted
  from `cache::VERSION`** — bumping the on-disk cache format version broke
  `offsets.bin` reads silently (empty result, not an error) against any
  freshly-written cache. Now references `cache::VERSION` directly.

- **`cache::VERSION` had never been bumped since the disk-cache mechanism
  was introduced**, despite multiple report-model/triage changes landing
  since — meant a correct code change could silently appear to have no
  effect when tested against an already-cached dump. Bumped to 2 to cover
  this session's cumulative changes; see the new "Disk cache gotcha" note
  in `CLAUDE.md`.

## [0.3.2] — 2026-10-19

Several items in this release were cherry-picked from
[Pigsy-Monk/hprof-analyzer](https://github.com/Pigsy-Monk/hprof-analyzer/)
and are marked *(PM)* below.

### Added

- **`jsonify(x [, depth])` OQL function** — expands any heap object into a
  JSON tree up to `depth` levels deep (default 3). Cycles, depth cuts, and
  budget cuts are annotated with `@notExpanded`. Available in the REPL, MCP
  `query` tool, and `heap query` CLI. Requires `offsets.bin`. Not available
  in the web version. Object-read infrastructure cherry-picked *(PM)*.

- **`s[n]` / `s[a:b]` array subscript and slice** — `SELECT s[0] FROM int[] s`
  returns element at index 0; `s[1:4]` returns a JSON array. Requires
  `offsets.bin`. Not available in the web version.

- **`heap load` / `load_dump` write `offsets.bin`** — byte offset of every
  object written to `<dump>.hprof-cache/<hash>/offsets.bin`; `.hprof.gz`
  files also get a seekable block index (`gz_index.bin`). Enables `jsonify`
  and array subscript without re-scanning.

- **`inspect_object` / `heap inspect` show field values** — actual field
  values from the dump file, not just sizes.

- **`heap query --all` / MCP `query` `all=true`** — bypass GC-reachability
  filtering; useful for dumps taken without `:live`. *(PM)*

- **`@index` / `@class` attribute aliases** — aliases for `@objectId` /
  `@displayName`, matching `jsonify` compact-ref output keys.

- **`hprof-analyzer check-redacted <INPUT>`** — exits 0 if the dump contains
  the redaction marker, 1 otherwise, 2 on I/O error.

- **`hprof-redact --check <INPUT>`** — same check in the standalone binary.

- **`detect-secrets` scans JVM symbol table strings** — `STRING_IN_UTF8`
  records (annotation values, `@RequestMapping` paths, JDBC URLs, etc.) are
  now scanned alongside heap strings. *(PM)*

### Fixed

- **`heap query` / MCP `query` reachability filter was broken** — opts
  mismatch caused `reachable_only=false` regardless of the flag.

- **Cross-phase queries dropped unreachable rows incorrectly** —
  `filter_result_by_src` was applied even when `reachable_only=false`.

- **`heap query` ignored `--reachable-only`** — flag was not threaded into
  `arm_row_capture`.

- **`LIMIT` not consumed by the scan** — `plan.scan_limit` was set but never
  read; `LIMIT N` only trimmed post-scan instead of stopping early.

- **`pass2` inbound-CSR rescan path dropped edges** — CSR rebuild bug caused
  silently omitted edges, producing incorrect retained-size and dominator results.

- **`cvec` ignored the requested deflate level** — always used level 6.

- **`redact --complete`: annotation literals and Spring EL expressions now
  zeroed** — non-structural `STRING_IN_UTF8` entries are zeroed; structural
  names (class, field, method, source file) are preserved. *(PM)*

- **`pass2::fill_heap` pegging one core on clustered heap addresses** —
  stagnation guard added to interpolation search; falls through to binary
  search when a probe fails to halve the range. *(PM)*

- **Unreachable-forest dominator pass auto-skipped above 500 000 unreachable
  objects** — shows shallow-by-class instead; `--full-analysis` forces the
  full pass. *(PM)*

- **MAT `.threads` file now matches Eclipse MAT exactly** — real addresses,
  complete locals list, correct empty-frame placeholder. *(PM)*

- **Unreachable class objects have their own shallow size** — previously
  bucketed incorrectly in the Unreachable Objects histogram. *(PM)*

- **Alias histogram rows folded into one `ClassImpl`** — duplicate entries
  and incorrect retained totals in the MAT class cache are fixed. *(PM)*

- **`FROM OBJECTS <array>` queries served from the resident cache.** *(PM)*

- **Server refuses dump re-reads by default; `--allow-rescan` opt-in.**
  `--no-rescan` kept as a deprecated no-op. *(PM)*

- **Six misleading report renderings corrected.** *(PM)*

- **`graph.bin` forward targets stored as a chunked container** — reduces
  peak RSS when building large MAT caches. *(PM)*

- **Inbound CSR read at point of use, not up front** — cuts resident memory
  for queries that never touch inbound edges. *(PM)*

- **From-cache path no longer materialises the whole graph cache.** *(PM)*

## [0.3.1] — 2026-10-02

### Added

- **Browser UI: `?file=<url>` query parameter.** Opening the browser UI with
  `?file=<url>` auto-loads without user interaction. A URL ending in `.hprof`,
  `.gz`, or `.zip` is fetched and loaded via WASM; any other URL is treated as
  a server base URL and auto-connected. Enables plugin-launched workflows such
  as `hprof-analyzer server dump.hprof` opening
  `https://parttimenerd.github.io/hprof-analyzer/?file=http://127.0.0.1:7070`.

### Changed

- **`hprof-redact` moved to its own workspace crate (`crates/hprof-redact`,
  `publish = false`).** The binary no longer appears in `cargo install
  hprof-analyzer`; it is built and distributed exclusively by CI.

- **MCP: `get_summary` tool removed.** It was redundant with the `load_dump`
  response, which now embeds a ready-to-run OQL query for the top leak suspect.
  Agents should call `get_report({section:"triage"})` or `get_report({section:"leaks"})`
  instead.

- **MCP: `get_report({section:"top-classes"})` default limit reduced from 20 to 10.**
  Each top-classes row carries holder breakdown data, making 20 rows
  disproportionately large. Pass `limit` explicitly for more rows.

- **MCP: dominator tree width capped at 20 children per node in leaks section.**
  Previously a single node with thousands of children could produce multi-MB
  responses. Truncated nodes carry `{"_truncated":true,"_omitted":<n>}`.

- **MCP: `get_report` section routing clarified.** Tool description now
  explicitly separates "simple question" sections (`top-classes`, `top-objects`,
  `overview`, `threads`) from "leak investigation" sections (`triage`, `leaks`,
  `retainers`, `dominators`), reducing unnecessary `triage` calls for histogram
  lookups.

## [0.3.0] — 2026-09-09

### Added

- **Lean redaction mode (new default for `hprof-redact`).** `RedactMode::Lean`
  is a single-pass redactor that zeroes all primitive array element data
  (`byte[]`, `char[]`, `int[]`, `long[]`, etc.) while copying `INSTANCE_DUMP`
  and `CLASS_DUMP` bodies verbatim. No class-field map is built; no HashMap is
  allocated. Use when throughput matters and leaking scalar fields (e.g.
  `String.hash`) is acceptable.

- **Complete redaction mode (opt-in via `--complete`).** `RedactMode::Complete`
  is the original two-pass mode, renamed and made opt-in. It zeroes all
  primitive values everywhere: array elements, scalar instance fields, and
  CLASS_DUMP static values. Use when the strongest privacy guarantee is needed.

- **`redact-bin` Cargo feature.** Minimal dependency set for the standalone
  `hprof-redact` binary (`dep:zip` + `dep:tar` only). Excludes clap, tokio,
  reedline, rmcp, scraper, ureq, self-replace, crossterm, anyhow, and bincode.
  Keeps the binary under 1 MB on all platforms.

- **`hprof-redact` binary now defaults to lean mode.** Pass `--complete` for
  full two-pass redaction. Progress output is suppressed when writing to stdout
  (piping-friendly).

- **MCP `redact` tool: optional `complete` parameter.** `complete: false`
  (default) uses lean mode; `complete: true` uses complete mode. The result
  message describes what was zeroed vs. preserved.

- **WASM: `redact_with_progress(data, name, mode, cb)`.** The `mode` parameter
  accepts `"lean"` or `"complete"`. Previous callers without the parameter must
  be updated.

- **Browser: Redact modal.** The Redact button opens a dialog to choose lean
  vs. complete mode before downloading the redacted dump.

- **ZIP streaming in `open_zip` / `open_zip_bytes`.** ZIP input now streams
  the decompressed HPROF content on-the-fly instead of buffering the entire
  decompressed file in memory (same `Box::leak + transmute` pattern as the
  existing tar path).

### Fixed

- **`copy_bytes` heap allocation eliminated.** The inner copy loop previously
  allocated a `vec![0u8; 65536]` per call. Now delegates to `copy_exact` which
  uses a stack `[u8; 4096]` buffer.

## [0.2.2] — 2026-09-04

### Added

- **`redact` subcommand and `hprof-redact` standalone binary.** Zeroes all
  primitive field values and array element data (char[], byte[], int fields,
  etc.) while preserving the complete object graph (class/field names, object
  IDs, reference links). Output is readable by hprof-analyzer, Eclipse MAT,
  and jhat. Accepts `.hprof`, `.hprof.gz`, `.hprof.zip`, `.tgz` input; output
  format is inferred from extension.

- **Redaction marker record (tag `0xDE`).** Redacted dumps carry a
  `REDACTED\x01` top-level record so hprof-analyzer can detect them and show
  a "Redacted dump" info banner in reports. Old tools skip the record cleanly.

- **`redacted_input: true` in JSON reports.** When a redacted dump is loaded,
  the report JSON includes `redacted_input: true` (mirrors `truncated_input`).
  Duplicate-string and collections analyses are skipped (they return
  meaningless all-zero results on zeroed data).

- **In-memory ZIP support for `HprofSource::Bytes`.** WASM and test code can
  now pass `.hprof.zip` bytes directly; the ZIP is decompressed in-memory
  without a temporary file.

### Fixed

- **Redactor handles `HEAP_DUMP_INFO` sub-records (tag `0xFE`).** The
  sub-record was previously unhandled, causing the heap-segment loop to
  desynchronize and corrupt or truncate output for dumps containing multiple
  heap segments with info records (Android, some HotSpot dumps).

- **Unknown sub-tag drains remaining segment bytes.** Previously an unknown
  sub-tag caused a silent early exit from the segment loop, leaving the
  outer stream desynchronized. Now remaining bytes in the segment are consumed
  before breaking, so subsequent top-level records parse correctly.

- **Conservative zeroing for unknown-class instances.** Instance blobs whose
  class is not in the field-type map (e.g. class was in a skipped segment) are
  now zeroed entirely rather than copied verbatim, preventing data leakage.

- **Redactor no longer over-reads instance blobs.** Added a `written >=
  data_len` guard that stops field processing if accumulated field sizes
  exceed the reported blob size; tail bytes are zeroed rather than copied.

## [0.2.1] — 2026-09-03

### Added

- **Setup & Usage modal in the browser UI.** Clicking "Setup & Usage" in the
  footer opens a modal with install instructions (Homebrew, Cargo, prebuilt
  binary), CLI quick reference, and MCP setup for Claude Code, Cline, and
  Claude Desktop. All code blocks are syntax-highlighted (shell and JSON).

- **Collapsible install panel on the browser landing page.** "— or install it
  locally (and with coding agents) —" expands to show CLI install tabs and AI
  integration cards inline, without leaving the page.

- **OQL syntax highlighting in the browser shell.** Both the sidebar query
  preview (HTML spans) and the xterm.js terminal input (ANSI codes) now
  highlight keywords, strings, numbers, identifiers, and `--` comments.

- **Shell and JSON syntax highlighting in setup guide code blocks.** Shell
  commands are tokenised into command/subcommand/flag/string/path/comment
  classes; JSON config blocks highlight keys, string values, keywords, and
  numbers. Both follow the page's light/dark theme.

- **Sample dump cards always visible on the landing page.** Sample dumps
  (mnemonics, scala-doku, philosophers, gauss-mix) are shown at all times;
  cards are grayed out when the server does not serve the files (e.g. local
  `file://` open) and become clickable once probed as available.

- **`mat caches` in CLI usage guides.** The setup modal, install panel,
  and README quick reference now include `hprof-analyzer mat caches dump.hprof`
  as a key command.

### Fixed

- **MCP: OQL alias `AS RETAINED` conflicted with reserved keyword.** All
  generated queries (examples, `get_summary`) now use `AS ret` / `AS ret_bytes`
  instead of `AS retained` / `AS RETAINED`.

- **MCP: `browse_dominators` / `query` responses had a trailing `/* HINT */`
  comment** that broke `json.loads()` for every model calling the tool. The
  hint is now in a `_hint` JSON field.

- **MCP: `get_report` now exposes all 17 report sections** with documentation
  updated to match actual JSON field names.

- **MCP: named queries use `@objectId AS idx`** instead of `@objectAddress`,
  so results plug directly into `browse_dominators` / `inspect_object` without
  address conversion.

- **CLI: `heap cache-list` now accepts a `.hprof` file path** (previously
  failed when passed a file rather than a directory).

- **MCP: leak detection fixed for small/medium heaps.** `load_dump` now embeds
  the top-5 suspect list in its response; `NEXT STEPS` now point to
  `get_report({section:"leaks"})` as the first action rather than the OQL
  `leak-suspects` view, which has a >10 MB threshold and returns 0 rows on
  small/medium heaps.

### Changed

- **OQL footguns documented:** glob patterns do not match inner classes
  (`ZipFile*` misses `ZipFile$Source` — use exact name or `INSTANCEOF`);
  `retained` / `RETAINED` column alias conflicts with `AS RETAINED SET`;
  `@objectId` is 0-based while `obj_index_1based` from `get_report` is 1-based.

- **MCP `get_report` description** updated: calls it "BEST TOOL for finding the
  leak / why OOM", explains single vs. group suspect JSON structures, and
  clarifies that the `leak-suspects` OQL view has a >10 MB threshold.

- **README "Browser mode" section removed.** Content was stale and duplicated
  the "Try it in the browser" section above it.

## [0.2.0] — 2026-08-10

### Added

- **MCP server.** Exposes heap analysis via the Model Context
  Protocol so Claude Code, Cline, Claude Desktop, and any other MCP-compatible
  AI assistant can load and analyze heap dumps interactively. Tools: `get_session_info`,
  `get_oql_docs`, `load_dump`, `get_summary`, `get_histogram`, `get_report`, `query`,
  `browse_dominators`, `inspect_object`. The server is stateful — `load_dump` must be
  called first, after which all other tools use the same session. Optionally pre-load
  a dump with `hprof-analyzer mcp --dump heap.hprof`.

- **`heap` subcommand group.** Every MCP tool is also a CLI command for scripting
  and LLM-driven workflows:
  ```
  hprof-analyzer heap summary <dump>
  hprof-analyzer heap histogram <dump> [--limit N] [--json]
  hprof-analyzer heap report <dump> [--section leaks|top|threads|overview|all] [--json]
  hprof-analyzer heap query <dump> --oql "..." [--json]
  hprof-analyzer heap browse <dump> [--index N] [--depth D] [--width W] [--json]
  hprof-analyzer heap inspect <dump> --index N [--json]
  hprof-analyzer heap docs [--topic syntax|attributes|examples|workflow|all]
  hprof-analyzer heap load <dump> [--with-graph]
  hprof-analyzer heap cache-list [<dump>]
  hprof-analyzer heap cache-clear <dump>
  ```
  Output is human-readable text by default; `--json` for machine-parseable output.

- **Disk cache.** The first analysis of a dump writes a cache to
  `<dump>.hprof-cache/<hash>/` (5–15 min, 70–400 MB). Every subsequent call for
  the same dump loads in ~1 s. Cache is content-addressed (first 64 bytes + file
  size + mtime) and busts automatically on dump changes. Add `--with-graph` on the
  first load to cache the reference graph for OQL `@inbounds`/`@outbounds` traversal
  (adds 200–600 MB).

- **Homebrew tap.** Install via:
  ```sh
  brew tap parttimenerd/hprof-analyzer
  brew trust parttimenerd/hprof-analyzer
  brew install hprof-analyzer
  ```
  A rolling nightly formula tracks every push to `main`; a stable versioned formula
  is published on each tagged release.

- **Holder breakdown in HTML report.** The Biggest Classes section now shows
  a collapsible "Held by (immediate dominators)" widget. For each top class,
  it lists the immediate-dominator classes (with instance count and retained
  size) and their own immediate dominators (level 2), making it easy to
  answer "which classes are keeping all these HashMaps alive?" without
  leaving the Top Consumers section. Computed via two O(n) passes over all
  objects after the class ranking is built; capped at 10 holders per class
  and 10 level-2 entries per holder.

- **`holder_chain` on biggest objects.** `ObjRow` now carries up to 2
  immediate-dominator class names toward the GC root, available in the JSON
  model for tooling that consumes the report programmatically.

- **Truncation resilience.** Truncated dumps, corrupt gzip streams, and
  malformed heap records all produce a partial report with a `truncated_input`
  warning rather than a crash or error exit. Corrupt length fields are capped
  before allocation to prevent OOM. Validated with a proptest fuzzing suite
  (8 suites × 500 cases) and a 10 000-iteration prefix-fuzz campaign.

- **`.tar.gz` / `.tgz` input support.** Heap dumps packaged as gzip-compressed
  tar archives are accepted everywhere a plain `.hprof` or `.hprof.gz` is —
  CLI, `diff`, `server`, `query`, and `mat` subcommands. Streamed on-the-fly;
  no decompression to disk required.

- **Android ART and IBM J9 support.** All HPROF sub-tags from Android ART
  (`ROOT_INTERNED_STRING`, `ROOT_DEBUGGER`, `ROOT_VM_INTERNAL`, `ROOT_JNI_MONITOR`,
  `PRIM_ARRAY_NODATA_DUMP`) and IBM J9 (`ROOT_SYSTEM_CLASS`) are now parsed
  correctly. Previously these produced `unknown heap sub tag` errors and aborted.

- **Interactive Object Graph Explorer.** Force-directed graph view for leak
  suspects and dominator tree nodes, powered by d3-force. Includes Inspector
  integration, neighbor dimming, and edge labels.

- **Heap Inspector panel.** Click-through from any object to see its fields,
  inbound references, retained size, and per-field value scan (top instances by
  retained size).

- **ThreadLocal Analysis section** in Markdown reports, plus Collection Waste
  Budget section with TOC.

- **`--field-stats` flag** for per-class reference-field null/non-null/retained
  breakdown.

- **WASM memory optimizations.** Compressed `shallow`/`retained`/`fwd_targets`
  arrays and a `run_fast_analysis_with_progress()` path that skips the retained
  pass — enables large dumps to load in-browser without OOM.

### Changed

- Holder breakdown is omitted from the plain Markdown renderer (it is
  present in the JSON model and HTML report only).

### Fixed

- `inspect_object` no longer panics when called with `with_graph=true` if
  the inbound CSR was not cached (the graph cache stores forward edges only;
  a bounds-check now guards the inbound lookup).

- All scan loops in pass 1, pass 2, and the field-decode layer now treat
  `InvalidData` (corrupt sub-tag, segment overrun, oversized array) identically
  to `UnexpectedEof` — stop scanning and emit a partial report instead of
  propagating a hard error.
- Saturating arithmetic throughout shallow-size helpers, collection-waste
  calculations, and array byte-length computations prevents integer overflow on
  file-derived values.
- Numerous HTML report display fixes: number formatting, percentage rendering,
  exact-byte tooltips, table overflow, column truncation.

"""Implementation notes for agents, keyed by idea ID.

Each entry: data (existing report inputs), rust (backend change), ui (frontend
change), schema (proposed JSON contract, TypeScript style), test (acceptance).
"rust": "none" means a pure frontend task over existing report fields.
Paths are relative to the repo root. Verify field names in
src/report/model.rs and web/src/types.ts before coding; they are the truth.
"""

# Conventions every agent must follow; rendered once at the top of the page.
COMMON = [
    "Read CLAUDE.md and the selected idea's source section first. Treat these notes as a handoff, not a verified API contract.",
    "New report fields: add to src/report/model.rs as <code>Option&lt;T&gt;</code> or <code>Vec&lt;T&gt;</code> with "
    "appropriate serde defaults/skips; mirror in web/src/types.ts and regenerate the canonical "
    "schema/report.schema.json using the test in src/report/mod.rs. Check docs/schema.json if it is separately consumed.",
    "Any change to what build_model/evaluate_triage produce: bump <code>VERSION</code> in src/cache.rs, "
    "or cached reports silently hide the change.",
    "Prefer bounded aggregation in the relevant existing pass (e.g. src/pass2/fielddecode.rs or "
    "src/report/build.rs); verify data lifetime and memory before adding any pass.",
    "Cap every list (default 20–50 rows, honour <code>--detail</code> via src/opts.rs) and record truncation "
    "(<code>truncated: bool</code> or <code>omitted_count</code>).",
    "Single report rule: distinguish pass not run, feature not in this dump, zero observed, and truncated. "
    "Hide unavailable views but leave coverage evidence visible; never guess values.",
    "Only sum shallow bytes. Retained bytes overlap; label them and never add them up.",
    "Regression tests: use tests/scenarios.rs and tests/common/heap_builder.rs for new dump-derived signals; "
    "for UI-only changes add frontend tests for shown/hidden states and metric labels.",
    "UI: reuse exported ZoomableTreemap/RetainedTreemap or existing wrapper components in web/src/charts.tsx. "
    "HBar/VBar/StackedBar/Pie there and StdTable/CappedTbody/PivotBtn/ExploreBtn in web/src/App.tsx "
    "are private helpers: export/refactor deliberately rather than importing them as-is.",
    "Web build check: <code>cd web &amp;&amp; npm run build</code>; open a generated report and a report without the data "
    "to verify hiding.",
]

N = "none (existing report data)"

NOTES = {
    # ---------------- Pools and finalization ----------------
    "P1": dict(
        data="Proposed: decoded fields from verified ThreadPoolExecutor/ForkJoinPool and framework-specific layouts; current framework scan is class footprint only.",
        rust="Start with a supported JDK ThreadPoolExecutor layout in fielddecode.rs. Decode core/max, "
             "workers, and workQueue size only when the class+field layout is verified; add other pools "
             "individually. Unknown layout -> 'not decoded', not zero or 'idle'.",
        ui="'Pool state' table in the Threads section: pool, class, core/max, workers, queued, signal pill.",
         schema="PROPOSED pools?: { cls: string; obj_id: string; core?: number; max?: number; workers?: number; queued?: number; "
             "decode_status: string; truncated?: boolean }[]",
        test="Scenario: TPE with 10k queued Runnables -> row with queued=10000 and triage 'Queued work retention'."),
    "P2": dict(
        data="Existing registry count; PROPOSED: referents of verifiably decoded Finalizer records.",
        rust="Do not assume the Soft/Weak/Phantom REF_CLASSES decoder handles FinalReference. Check "
             "JDK-specific Finalizer/FinalReference referent layout; group only resolved referents by class "
             "and count unresolved separately. Never call it a queue backlog without queue decoding.",
        ui="Table under N2: referent class, count, shallow; link via PivotBtn.",
        schema="PROPOSED finalizer_referents?: { cls: string; count: number; shallow: number }[]; unresolved?: number",
        test="Scenario: 20k objects with finalize() -> top row is that class, count 20000."),
    "P3": dict(
        data="PROPOSED: supported JDK Cleaner/PhantomCleanableRef or sun.misc.Cleaner registrations.",
        rust="Decode supported cleaner action/layout; phantom referents may be cleared, so do not promise "
             "referent-class attribution. Group by decoded action, with unknown referent count.",
        ui="'Cleaner registrations' table next to P2.",
        schema="PROPOSED cleaners?: { referent_cls?: string; action_cls: string; count: number; unresolved: number }[]",
        test="Scenario: supported cleaner layout -> action count; cleared referents remain unknown."),
    "P4": dict(
        data="DirectByteBuffer capacity already decoded in fielddecode.rs.",
           rust="Only for verified JDK layouts: inspect cleaner/att graph and deduplicate by supported "
               "backing identity; a non-null cleaner alone is not proof of unique native allocation. "
               "If unresolved, emit no unique figure.",
        ui="Two numbers on the B4 header: 'allocated (unique)' and 'all buffers'.",
        schema="PROPOSED direct_buffer_capacity_unique?: number; decode_status?: string; existing leak_indicators.direct_byte_buffer_capacity_sum",
        test="Scenario: 1 MB buffer + 100 slices -> unique 1 MB, all 101 MB."),

    # ---------------- Ownership and paths ----------------
    "A1": dict(data="leaks.suspects[].dominator_tree (bounded).", rust=N,
               ui="Optional sunburst mode on the existing ZoomableTreemap data; breadcrumb + 'self + unshown' slice; "
                  "≤200 nodes.",
               schema="reuse DominatorNode { cls; retained; shallow; children[] }",
               test="UI: sum of children + self+unshown equals parent retained."),
    "A2": dict(data="dominator_analysis.immediate_dominators.pairs (capped; pairs_truncated).", rust=N,
               ui="Searchable table owner class × payload class; optional 10×10 heatmap.",
               schema="EXISTING ImmDomPair { dominator_class; dominated_class; pair_count; dominated_shallow; dominated_retained }",
               test="UI: filter by class shows only matching rows."),
    "A3": dict(data="EXISTING dominator_analysis.big_drops.rows; bounded dominator_tree for selected suspects.", rust=N,
               ui="Show retained - largest_child_retained as drop, not proven savings; link to selected owner.",
               schema="EXISTING BigDropRow { retained; largest_child_retained; drop_bytes }",
               test="UI: drop_bytes equals retained minus largest_child_retained; no additive siblings claim."),
    "A5": dict(data="Bounded dominator_tree nodes with same-class parent/child; longest_chain_depth is global, not per-class.", rust=N,
               ui="Flag same-class runs only within the captured tree; label observed depth as a lower bound.",
               schema="derived from bounded DomTreeNode; class-wide longest run PROPOSED",
               test="Long chain outside the tree cap must not be reported as a measured class-wide depth."),
    "B1": dict(data="dominator_analysis.immediate_dominators.pairs for A2; type_ref_graph requires graph capture and has non-conserved retained_weight.", rust=N,
               ui="Prefer A2's sortable pair table. Do NOT draw a conserved Sankey from type_ref_graph; a flow needs an explicit additive partition.",
               schema="EXISTING ImmDomPair pair_count/dominated_shallow; no conserved flow field",
               test="UI: never turn retained_weight into flow width or assert its links add to heap size."),
    "B2": dict(data="obj_graph_flat inbound refs + dominator.",
               rust="Optional: in query/ add 'referrers' query returning up to 20 inbound refs per object.",
               ui="Owner chain + 'other referrers' list in the X2 inspector.",
               schema="referrers?: { obj_id; cls; field?: string }[]",
               test="Scenario: object held by 2 maps -> both listed."),
    "B3": dict(data="EXISTING GC root type counts and qualified root retained rows.", rust=N,
               ui="Show counts by root type; if displaying retained attribution, use separate bars/table (overlapping, not stacked).",
               schema="existing root-type counts / GcRootsRetainedChart",
               test="UI: never stack overlapping root retained values into a total."),
    "B4": dict(data="EXISTING leak_indicators.nio_buffer_owners + direct_byte_buffer_capacity_sum; P4 is independent.", rust=N,
               ui="Existing owner root-path table. Label capacity sum as possible slice double-count; add verified P4 figure later.",
               schema="EXISTING NioBufferOwner[] { capacity; obj_index_1based; display_class; retained; root_path }",
               test="Scenario: Netty pool owning 50 buffers -> top owner row."),
    "K1": dict(data="top_retainers (RetainerRow).", rust=N, ui="HBar + table, PivotBtn/ExploreBtn per row.",
               schema="existing", test="UI: rows sorted by retained desc."),
    "K2": dict(data="fields_by_size (FieldsBySize).", rust=N, ui="Table field → retained; group by declaring class.",
               schema="existing", test="UI: hidden when fields_by_size is null."),
    "K4": dict(data="suspect path + root/field names.", rust=N,
               ui="Annotate path nodes with measured facts (field, root type); neutral colours only.",
               schema="existing", test="UI: no 'leak'/'safe' colouring by package."),

    # ---------------- Consumers and distributions ----------------
    "C1": dict(data="EXISTING category_rollup: additive class-category own shallow; classification comes from classify.rs.", rust=N,
               ui="Stacked shallow-only category bar; CategoryFilterBar for drill-down.",
               schema="EXISTING category_rollup[]", test="UI: disjoint shallow categories sum to their covered total."),
    "C2": dict(data="framework_analysis (FrameworkAnalysis).",
               rust="Extend framework_scan.rs: Caffeine BoundedLocalCache maximum, Guava LocalCache maxWeight, "
                    "Spring ConcurrentMapCache (unbounded) -> decoded config + size.",
               ui="Framework table with 'bounded: yes/no/unknown' column.",
               schema="caches?: { cls; obj_id; size: number; max?: number; bounded: 'yes'|'no'|'unknown' }[]",
               test="Scenario: ConcurrentMapCache with 50k entries -> bounded 'no'."),
    "C3": dict(data="class histogram (count, shallow).", rust=N,
               ui="Scatter count × bytes/instance (chartSetup.ts scatter), log axes.",
               schema="existing HistRow[]", test="UI: tooltip shows class + both values."),
    "C4": dict(data="EXISTING top.size_distribution.buckets: power-of-two retained sizes of top-level dominators (not DepthBucket, not all objects).", rust=N,
               ui="Use top-level-dominator histogram with explicit scope, not an all-object size histogram.",
               schema="EXISTING TopSizeDistribution { buckets: { upper_bytes; count }[]; count; min; max; median; total }",
               test="Bucket counts sum to top.size_distribution.count, not heap object count."),
    "C5": dict(data="RetentionSummary top1/10/100.", rust=N, ui="Three milestone bars (ConcentrationChart).",
               schema="existing", test="UI: no interpolated curve."),
    "I2": dict(data="EXISTING top.biggest_packages PackageNode tree (pruned top-level dominators).", rust=N, ui="Use package tree as a scope filter; do not make a second default treemap.",
               schema="existing", test="UI: click drills into package."),
    "K3": dict(data="histogram rows.", rust=N, ui="Scatter with axis selector (count/shallow/retained).",
               schema="existing", test="UI: axis switch keeps selection."),
    "K5": dict(data="instances of a chosen class.",
               rust="New named query in src/named_queries.rs: group instances by a field value (String/int/enum), "
                    "top 50 groups, count + shallow + retained.",
               ui="Group-by-field panel: pick class + field; result table.",
               schema="QueryResult rows { value; count; shallow; retained }",
               test="Scenario: 3 tenants × sessions -> 3 groups with right counts."),

    # ---------------- Collections ----------------
    "D1": dict(data="EXISTING waste_summary + waste_breakdown (opportunities can overlap).", rust=N,
               ui="Merged into J5: ranked independent opportunities, not a subtractive waterfall.",
               schema="EXISTING waste_breakdown[]", test="UI: no summed guaranteed reclaimable remainder."),
    "D2": dict(data="collections analysis (capacity, size).", rust=N,
               ui="Paired bars used vs capacity per collection class.", schema="existing CollectionsAnalysis",
               test="Scenario 18/20 leak-demos -> HashMap/ArrayList rows."),
    "D3": dict(data="strings analysis (Latin-1/UTF-16, duplicates).", rust=N,
               ui="StackedBar encoding + duplicates table.", schema="existing", test="UI: hidden when no strings data."),
    "D4": dict(data="collection_attribution by owning field.", rust=N,
               ui="Table field → empty/tiny collections, overhead bytes.", schema="existing CollectionAttribution",
               test="UI: shown only with collections enabled."),
    "D5": dict(data="collection_contents.", rust=N, ui="Stacked bar of element classes per collection kind.",
               schema="existing", test="UI: hidden when null."),
    "J1": dict(data="pass2/boxed.rs output.", rust=N, ui="HBar of boxed classes with 'cached range' note.",
               schema="existing", test="Scenario 26 -> Integer top."),
    "J2": dict(data="EXISTING overview.header_overhead[] contains the analyzer's estimated header bytes and share; this is not a measured JVM layout.", rust=N,
               ui="Display existing estimated header/payload fraction with model assumptions; no universal compressed-oops inference from ID width.",
               schema="EXISTING HeaderOverheadRow { instances; header_bytes; total_header_bytes; header_pct_of_shallow_bp }",
               test="UI labels assumptions and never adds header estimate to reclaimable waste."),
    "J3": dict(data="dup_prim_arrays.rs.", rust=N, ui="Table: fingerprint, copies, wasted shallow.",
               schema="existing", test="Scenario 22 -> group with copies."),
    "J4": dict(data="EXISTING collections.constant_primitive_arrays.rows with array_class, length, value, objects, shallow; truncated flag.",
               rust=N, ui="Table existing constant-array groups; do not reimplement in dup_prim_arrays.rs.",
               schema="EXISTING collections.constant_primitive_arrays { rows: ConstantArrayRow[]; truncated }",
               test="Scenario: constant byte arrays -> row; a capped group shows truncated."),
    "J5": dict(data="waste_summary, duplicates, boxed, collections.", rust=N,
               ui="Single ranked opportunity bar list with links to detail views.", schema="derived",
               test="UI: each row links to its section."),
    "J7": dict(data="Throwable instances + stackTrace arrays.",
               rust="Group Throwable by class + detailMessage; sum stackTrace/backtrace shallow.",
               ui="Table exception class, count, stored trace bytes.", schema="throwables?: { cls; count; shallow }[]",
               test="Scenario: 10k stored exceptions -> row."),
    "L1": dict(data="collections by kind.", rust=N, ui="StackedBar per kind.", schema="existing", test="UI: sums."),
    "L2": dict(data="EXISTING collections.map_collision_ratio (aggregate occupancy); per-map chain details are not emitted.",
               rust="No Rust for existing aggregate. Only add an on-demand bounded per-map query if real chain-depth evidence is needed.",
               ui="VBar existing aggregate bucket occupancy; do not label it per-map chain depth.",
               schema="EXISTING collections.map_collision_ratio; per-map query PROPOSED",
               test="Aggregate histogram labels match source; no 'max chain depth' without decoding chains."),
    "L3": dict(data="EXISTING collections.top_prim_arrays/top_obj_arrays.top_individual; arrays_by_size contains length histograms, not individual owners.",
               rust=N, ui="HBar largest individual arrays by own shallow + first observed owner field.",
               schema="EXISTING TopArrayRow { array_class; length; shallow; obj_index_1based; owner? }",
               test="UI: first field owner is labelled as such, not unique owner."),
    "L4": dict(data="biggest_collections.", rust=N, ui="Table with owner path button.", schema="existing",
               test="UI: hidden when null."),
    "N1": dict(data="EXISTING collections.top_prim_arrays/top_obj_arrays and arrays_by_size length histograms.", rust=N, ui="Large array facts; no G1 humongous claims.", schema="existing",
               test="UI: text never says 'humongous region'."),
    "N2": dict(data="Finalizer count.", rust="See P2.", ui="Count + P2 table.", schema="existing",
               test="UI: no 'pending queue' claim."),

    # ---------------- Threads and references ----------------
    "E1": dict(data="threads.state counts.", rust=N, ui="StackedBar thread states.", schema="existing",
               test="UI: hidden when no thread records."),
    "E2": dict(data="EXISTING thread_local_analysis is grouped by value class, not by individual thread; null-key total is in leak_indicators.", rust=N,
               ui="Table value class, stale/null-key entry count, attributed retained; use separate thread root data to investigate worker names.",
               schema="EXISTING ThreadLocalLeakRow[]", test="Scenario 05/37 -> stale value class row, not fabricated per-thread counts."),
    "E3": dict(data="threads retained.", rust=N, ui="HBar per thread retained.", schema="existing",
               test="UI: thread name + retained."),
    "E4": dict(data="stack traces + locals.",
               rust="Group threads by top N frames (signature hash); count per group.",
               ui="Table frame signature, threads, retained.", schema="thread_groups?: { frames: string[]; threads; retained }[]",
               test="Scenario 38 -> one large group."),
    "E5": dict(data="ROOT_MONITOR_USED roots.", rust=N, ui="Table monitor object class, count.",
               schema="existing", test="UI: no deadlock claim."),
    "I1": dict(data="references analysis.", rust=N, ui="Reference-type count bars or independent tables; referents may be shared or cleared.",
               schema="existing ReferencesAnalysis", test="Scenario 12 -> soft dominant."),
    "N4": dict(data="threads retained.", rust=N, ui="Merged into E3.", schema="existing", test="—"),

    # ---------------- Loaders, comparison, evidence ----------------
    "F1": dict(data="loader rollup (LoaderRollupChart).", rust="Decode ClassLoader.parent; count classes with 0 instances.",
               ui="Indented loader tree with class counts.", schema="loaders?: { id; cls; parent?: string; classes; zero_instance }[]",
               test="Scenario 08/36 -> two webapp loaders."),
    "F2": dict(data="class names per loader.", rust=N, ui="Table class name loaded by >1 loader.",
               schema="existing ClassloaderLeak data", test="Scenario 36 -> duplicates listed."),
    "F3": dict(data="class super ids.", rust="Sum shallow per superclass (walk super chain).",
               ui="Tree superclass → subclasses with shallow.", schema="hierarchy?: { cls; super?: string; shallow }[]",
               test="Sums of children ≤ parent rollup."),
    "G1": dict(data="multiple comparable reports (diff_reports.rs); ≥2 for delta, ≥3 for a meaningful trend.", rust=N,
               ui="Class counts/shallow deltas first; show sparkline only with ≥3 compatible captures.", schema="existing SeriesClassRow",
               test="UI: needs ≥2 reports."),
    "G3": dict(data="Two capped type_ref_graph lists; edges may be truncated and loader identity may differ.", rust="Diff only matching TypeEdge counts with explicit missing-edge/coverage status in diff_reports.rs.",
               ui="Table edge, Δcount, capture scope; never invent edge shallow or conserved bytes.", schema="PROPOSED edge_deltas?: { from; to; delta_count; coverage }[]",
               test="Two synthetic dumps -> growing edge first."),
    "H1": dict(data="triage + suspects with their existing ordered evidence.", rust=N, ui="Rank evidence without inventing numerical confidence/ROI.", schema="derived",
               test="UI: ties stable."),
    "H2": dict(data="EXISTING triage[] (producer registry order).", rust=N, ui="Digest: severity pill, one line, link; retain registry order unless explicit UI ranking is justified.",
               schema="EXISTING triage[]", test="UI: no unsupported severity=priority assumption."),
    "H3": dict(data="analysis_flags, truncated_input, redacted_input.", rust=N,
               ui="Coverage table: what was analysed / skipped.", schema="existing AnalysisFlags",
               test="UI: redacted dump shows warning."),
    "I4": dict(data="EXISTING alloc_sites?.traces_present + sites[].object_count; only real captured allocation traces qualify.", rust=N,
               ui="Allocation-site flame only when traces_present and usable frames; use object_count as additive width. Not a thread-stack flame.",
               schema="EXISTING AllocSites { traces_present; sites: AllocSite[] }",
               test="No traces -> hidden; with traces, sites count only surviving allocated instances."),
    "I3": dict(data="unreachable stats.", rust=N, ui="StackedBar unreachable by class.", schema="existing",
               test="UI: hidden when not computed."),
    "M2": dict(data="decoded system properties.", rust=N, ui="Key/value table; unknowns visible.", schema="existing",
               test="UI: no guessed -Xmx."),
    "N3": dict(data="overview + waste + triage.", rust=N, ui="KpiStrip (exists) with 4 tiles.", schema="existing",
               test="UI: each tile links."),

    # ---------------- Shared UX ----------------
    "X1": dict(data="any row.", rust=N, ui="tooltip.tsx: same fields everywhere (class, count, shallow, retained).",
               schema="—", test="UI: keyboard focus shows tooltip."),
    "X2": dict(data="obj_graph_flat.", rust=N, ui="Pinned inspector panel via ObjGraphCtx.", schema="—",
               test="UI: survives navigation."),
    "X3": dict(data="—", rust=N, ui="Right-click menu: Pivot, OQL, Explore, Copy.", schema="—", test="UI: Esc closes."),
    "X4": dict(data="—", rust=N, ui="Class name links everywhere via PivotBtn; NavHistoryCtx back.", schema="—",
               test="UI: back restores scroll."),
    "X5": dict(data="dominator chain / paths.", rust=N, ui="Path explorer reusing domTree.tsx.", schema="—",
               test="UI: expand lazy."),
    "X6": dict(data="paths to GC root.", rust=N, ui="Button opens X5 at object.", schema="—", test="UI: one click."),
    "X7": dict(data="—", rust=N, ui="Per-view interaction checklist.", schema="—", test="Review only."),
}


def for_id(i: str) -> dict:
    return NOTES.get(i, dict(
        data="See the idea text.", rust="Deferred; do not implement without re-entry criteria.",
        ui="—", schema="—", test="—"))

# hprof-analyzer — useful heap exploration, not a chart gallery

The product question is **“What occupies this heap, why is it still reachable,
and what evidence suggests an issue?”** A visualization earns space only when
it shortens that investigation. Big is not automatically bad; a category,
thread state, class name, cycle, or large retained/shallow ratio is not a leak verdict.

This review examines the **62 remaining original visualization ideas** (G4, N5,
N6 were removed) and the shared UX proposals. **54 ideas remain as core, merged,
or contextual slices; 8 are kept
in a separate new-views/ideas companion:
[VISUALIZATION_IDEAS_DEFERRED.md](VISUALIZATION_IDEAS_DEFERRED.md).** Merged IDs
are preserved for traceability, not counted as separate report panels.
Four new pool/finalizer ideas (P1–P4) close gaps found in a coverage check.
Every ID has a [user verdict](#user-verdicts); a one-page index is in
[VISUALIZATION_IDEAS_SUMMARY.md](VISUALIZATION_IDEAS_SUMMARY.md).

The [demo page](docs/visualization-demos.html) is a prototype with clearly
labeled mock data, not a change to the production report or Rust analysis.
It covers 13 of the 17 Must-have views (see [demo coverage](#demo-coverage)).
No new chart package is required.

## Navigation

- [Decisions and report layout](#decisions-and-report-layout)
- [Decision record (2026-11-04)](#decision-record-2026-11-04)
- [What Basic and Full actually mean](#what-basic-and-full-actually-mean)
- [Cost model and budgets](#cost-model-and-budgets)
- [Every original idea](#every-original-idea)
- [User verdicts](#user-verdicts)
  - [Demo coverage](#demo-coverage)
- [Pools and finalization](#pools-and-finalization)
- [Ownership and paths](#ownership-and-paths)
- [Consumers and distributions](#consumers-and-distributions)
- [Collections and optimization opportunities](#collections-and-optimization-opportunities)
- [Threads and references](#threads-and-references)
- [Loaders, comparison, and evidence](#loaders-comparison-and-evidence)
- [Shared exploration UX](#shared-exploration-ux)
- [Triage: improve evidence before adding warnings](#triage-improve-evidence-before-adding-warnings)
- [Implementation order and acceptance gates](#implementation-order-and-acceptance-gates)
- [Appendix: original idea catalog (superseded)](#appendix-original-idea-catalog-superseded)

## Decisions and report layout

**Hard constraint: use only heap-dump-derived evidence.** No external telemetry
(`jcmd`, RSS, GC logs, sidecars, or live profiler counters) is required to
compute or interpret findings in this catalog.

**Keep the report task-first, not one destination per ID.** The eight destinations
below are a proposed grouping, **not a hard cap**; use as many as navigation needs:

| Destination | Default answer | Contextual expansion / merged ideas |
| --- | --- | --- |
| Overview | Recorded reachable bytes, concentration, up to three evidence-backed findings | H1/H2/H3/N3/C5; no health score or guessed fix complexity |
| Consumers | Sortable class table, additive shallow bytes, largest instances | C1/C2/C3/C4/I2/F1/F2/F3/K3; package/category/loader are scope controls |
| Owners | Selected object's retained subtree and available ownership/root evidence | A1/A2/A3/A5/B1/B2/B3/K1/K2/K4/X5/X6; charts are modes, not more sections |
| Collections | Largest arrays/containers; capacity and structural overhead evidence | D1–D5/J1–J5/J7/L1–L4/N1; show relevant details, not eight simultaneous pies |
| Threads | Thread-object retained bytes, stack-local samples, selected frame, pool state | E1/E2/E3/E4/N4/P1; state is context, not severity |
| References | Soft/weak/phantom/final evidence, cleared/live referent states | I1/B4/E5/N2/P2/P3/P4 as conditional details; never equate capacity with unique native bytes |
| Compare | Signed class/count/shallow changes and comparable-capture trends | G1/G3; not shown without multiple reports |
| Evidence | Enabled passes, limits, redaction, record census, decoded environment | H3/M2 plus a collapsed existing census; unsupported is not “no issue” |

### Prevent confusion and visual overload

1. Start with facts plus **at most three distinct findings**. Group related rules
   by owner/issue; keep the rest behind “More evidence,” retaining access to every result.
2. One persistent scope and selection, with visible breadcrumbs and Reset.
   A click drills down; it must not silently change all denominators or erase unrelated evidence.
3. Prefer a ranked table with inline bars. Offer a small heatmap/scatter/subtree
   only where it answers the next question; no full-page global Sankey by default.
4. Retained bytes are nested/overlapping except in a declared disjoint ownership
   partition. Use **shallow** bytes for additive class/category composition.
5. A dominator relationship is not necessarily a direct reference/field edge.
   Label **ownership chain** versus **verified reference path** correctly.
6. “Parent retained minus largest child” includes siblings, own bytes, and
   omitted branches. It is not the parent's shallow size or guaranteed wasted memory.
7. Separate **observed footprint**, **estimated optimization opportunity**, and
   **conditional cut impact**. Never sum overlapping opportunities into an exact “reclaimable” total.
8. A capped scan and a complete scan with top-N output are different. Show scope,
   tracked/eligible counts where known, omitted output, and unknown coverage.
9. Use neutral category/state colors; reserve warning colors for evidence-backed
   issues. JDK/framework ≠ safe; application ≠ faulty; WAITING ≠ leak.
10. Redacted primitive data can invalidate String/array-content, collection-size,
    thread-state, and policy interpretations. Preserved graph/array lengths remain
    useful; do not run value-based inferences as if zeroed values were real.

## Decision record (2026-11-04)

This records the user's **catalog/planning decisions**, not implementation status.
No feature, producer, test, or String-accuracy work was authorized by these
approvals. The [idea verdict table](#user-verdicts) covers the earlier ID-by-ID
review; the refinements and explicit rejections below supersede conflicting
proposal text and the superseded appendix.

### Final catalog and shared-UX refinements

| Idea | Decision / conditions |
| --- | --- |
| F3 | Keep a future **selected application-family** rollup of direct/descendant counts and additive shallow bytes, not a global tree or summed retained totals; new heap-only hierarchy producer required. |
| G1 | Keep signed count/shallow-byte deltas and comparable-capture warnings; two points are a delta, not a trend or leak proof. No OOM forecast. |
| G3 | Keep type-edge deltas in selected-owner Compare detail only; show capture settings and cap/absence uncertainty. |
| H1 | Replace numeric fix ROI with at most three distinct evidence/impact/confidence/owner-specificity priorities, with expert override; do not guess fix difficulty or freed bytes. |
| H2 | Keep at most three concise evidence-linked statements, with observed/suggests/unknown language; do not force a culprit. |
| H3 | Keep a compact coverage summary and expandable actual-producer states; absence of a field does not prove a checked negative. |
| I3 | Keep unreachable composition in Evidence when substantial; disclose whether tree analysis was skipped; no fragmentation/GC diagnosis. |
| M2 | Keep a small masked, searchable, source-labelled property list in Evidence; unknowns stay unknown; do not infer JVM flags or capacity. |
| N3 | Replace a heap-health score with up to three neutral fact groups with explicit denominators and evidence links. |
| X1 | Keep short hover/**keyboard-focus** metric explanations. Reuse the existing selected-item sidebar/inspector for detail; do not duplicate it in tooltips. |
| X2 | Reuse/refine the existing inspector and consolidate overlapping sidebar surfaces; no second obstructive panel. |
| X3 | Optional small keyboard-accessible expert shortcut only if visible actions prove cumbersome; never the only way to inspect. |
| X4 | Keep explicit cross-links, selection, breadcrumbs/back/reset, and class-versus-object scope; do not silently filter every view. |
| X5 | Keep bounded, on-demand ownership/root-path exploration, distinguishing dominator relationships from verified reference fields. |
| X6 | Merge “Why retained?” into the inspector/X5; label representative-instance evidence as such. |
| X7 | **Side with the clutter concern:** share only essential metric/scope labels and inspect/link/copy conventions where relevant; no mandatory universal hover/context-menu/action checklist. |

**String content/slack/savings:** deeper physical accounting remains deferred in
[STRING_METRICS_IDEAS.md](STRING_METRICS_IDEAS.md); no UTF-8-derived Java storage
or reclaimability claims. Latin1-compatible wide-storage analysis stays there.

### Triage decisions

Correct existing wording before adding rules: `hash-collision-hotspot` is
bucket-occupancy evidence, not measured chain length/load factor;
`humongous-object-allocation` is a measured large primitive array, not proven
G1 classification; `finalizer-queue-backlog` is registered finalizer population
until a pending queue is verified; `gc-waste`/fragmentation is recorded
unreachable storage, not GC throughput or physical fragmentation; and
`thread-local-spread-leak` is worker-thread retention until stale values are
identified. Verify `Throwable` subtype before calling suffix-matched classes
exceptions (label name-only matches heuristic). Loader/buffer counts are
population/ownership facts, not metaspace, native-use, loader-lifecycle, or
physical-fragmentation diagnoses. String savings claims remain separately deferred.

| Candidate signal | User decision |
| --- | --- |
| Coverage-limited result | Keep **one grouped, evidence-linked notice** for meaningful input/scan/decoding limits; a top-N display cap alone is not incomplete analysis. |
| Shared-retainer caution | **Drop the proposed automatic caution.** B2's observed alternate-referrer evidence and explicit cut query remain; do not add a separate alert. |
| Stale ThreadLocal value footprint | Keep only as a verified, bounded refinement of E2; deduplicate distinct values, no new generic alert. |
| Repeated empty-field overhead | Keep as a field-specific refinement of existing D4/empty-container findings; no duplicate alert or guaranteed replacement advice. |
| Large array with actionable owner | Keep as an owner-specific refinement of the existing large-array finding, not another alert. |
| Business-bucket footprint skew | **Drop the proposed triage signal.** K5's on-demand business-field grouping remains available, without an automatic skew alert. |
| Application superclass-family footprint | Keep contextual F3 selected-family count/additive-shallow evidence, not an automatic leak alert. |
| Latin1-compatible wide storage | Defer with the separate String-metrics work; do not add a current triage rule. |
| Queued work retention | **Drop the proposed triage signal.** P1's descriptive, coverage-qualified pool/queue snapshot remains. |
| Finalizable population by class | Keep P2's class breakdown as a population refinement, never a backlog or stalled-thread verdict. |

**No automatic triage signal solely from:** reference cycles (collectible when
unreachable); WAITING/BLOCKED state (normal snapshot context); unknown cache
policy (show measured footprint instead); zero-instance classes (loader detail
only); a high retained/shallow ratio (selected-owner context only); object-ID/
address gaps (not physical fragmentation); hypothetical G1 regions (**drop**
inferred region classification; retain measured large-array facts); or a
time-to-OOM prediction (**drop**; show observed deltas/intervals only).

### Approved order and acceptance policy

1. Correctness first: metric/scope labels, real producer coverage, cached-profile
  checks, and reuse of the existing inspector before new views.
2. Improve existing Consumers/Owners slices and actionable collection/thread
  detail before new chart types.
3. Add selected contextual modes and comparison, not every chart on the overview.
4. Start new producers with P2 finalizer classes and P4 verified buffer backing,
  then P1 descriptive pool-state decoding for supported layouts; no queued-work alert.
5. Size expensive F3/selected map-bin/ownership/P3 work on ThinkStation first;
  keep encoding-correct String analysis in its separate deferred document.
6. Keep companion deferred ideas separate until dump-derived evidence, bounded
  schema, and measured cost justify re-entry.

**Acceptance:** record one decision and a “shown when” condition per idea (dropped
ideas: decision/reason and never shown); keep evidence gates/trade-offs/costs for
kept or deferred ideas, marking estimates as estimates. Acknowledge existing
tables; use actual CLI/MCP/browser capabilities rather than assuming “Full”
includes every producer. Reconcile additive shallow/disjoint totals, preserve
omitted tree mass, compare trends on common scales, and never sum overlapping
retained/savings claims. Keep navigation clear **without a destination-count
cap**; reuse one inspector and a short digest. Exercise affected keyboard,
Escape, responsive, reduced-motion, offline, and old-link flows when UI changes.
New Rust producers/rules need production-rule tests and **fresh ThinkStation
memory/runtime measurements** before being considered ready, not catalog
estimates. These are future implementation gates, not tests run in this review.

## What Basic and Full actually mean

The authoritative behavior is executable source, not stale option comments or
which optional field happens to be empty.

**Basic here means a fresh core report after dominator/retained computation.**
It is not the browser's initial parsed-only screen and is not “no heap analysis.”

| Entry point | Basic/core behavior | What its Full request enables | Not automatically enabled |
| --- | --- | --- | --- |
| CLI analyze | Default options; core graph/dominators, histogram, suspects, bounded ownership trees/paths, stack-local samples, collection summaries, reference stats, framework/loader/array data | `find_duplicates`, `collections`, `obj_graph`, `force_unreachable_dominator` | `ref_paths`, `field_stats`, and allocation-history detail that is absent from the dump |
| MCP `load_dump` | Default report; optional collections/duplicates; `with_graph` controls the query/cache graph | `collections` + `find_duplicates` | CLI-style report graph capture and forced unreachable-dominator override; `with_graph` is a separate query capability |
| Browser/WASM report | Core report when optional collection/duplicate inputs are false; initial load alone is not a retained report | The shown APIs set collection/duplicate inputs; `run_full_analysis()` sets both true | CLI `obj_graph`, `ref_paths`, `field_stats`, forced unreachable pass |

**Sources:** [CLI folding](src/main.rs#L2282-L2296),
[MCP folding](src/mcp/mod.rs#L336-L343),
[WASM Full](crates/hprof-wasm/src/lib.rs#L346-L356),
[WASM option API](crates/hprof-wasm/src/lib.rs#L557-L570),
[browser option forwarding](web-browser/shell.js#L1192-L1194).

### Gates used in the idea assessments

- **Basic:** already serialized in a fresh core report; charting it needs no Full pass.
- **Collections:** `--collections` or a Full request that sets it; some aggregate
  fill/size/kind/array data is **already Basic**. Owner/content detail is the optional part.
- **Duplicates:** `--find-duplicates` or the corresponding Full input. This also
  gates the value-class ThreadLocal breakdown; the null-key scalar is already Basic.
- **Report graph:** CLI `--obj-graph`/Full; captured/truncated, not the full heap graph.
  MCP query graph and a live WASM inspector are alternative capabilities, not identical JSON.
- **Custom:** a separate flag/query, such as `--field-stats` or `--ref-paths`; Full does not imply it.
- **New heap-only:** the dump contains candidate input, but a new producer/query
  is required. **Current Full does not implement a proposed pass.**
- **Multi-report:** compatible reports from more than one capture; not a single-dump flag.
- **Deferred:** specialist feature that is still dump-derived but not default; no suggestion to “just run Full.”

### Detail, size, and coverage are independent of Full

Current [default detail caps](src/opts.rs#L225-L248) are: root depth 30, top
allocation sites 50, local samples 20/thread, suspect subtree 5,000 nodes/depth
20, leak children 50, top consumers 20, and histogram paths for the top 20 rows.
The current options do **not** have separate `--thread-locals`/`--alloc-sites`
booleans: samples/aggregation run with positive detail caps. Older comments in
models still describe the former gating and should not drive UI copy.

[Thread sample collection](src/pass2/mod.rs#L1146-L1157) uses the cap;
[thread-object retained](src/report/build.rs#L2418-L2433) is read directly from
`g.retained`. The dominant-local class and significant locals are sampled; they
are **not** the source of the thread object's retained total.

`--size` controls graph/collection-detail caps and overrides the preceding graph
alias in the current CLI; its default is `default`. It does not enable passes.
Do not repeat the old “small is always the default graph cap” statement.

| Size | Graph edge cap/object | Attribution edge cap | Container cap | Node-KV cap | Slots sampled/collection |
| --- | ---: | ---: | ---: | ---: | ---: |
| small | 100 | 1,000,000 | 150,000 | 500,000 | 64 |
| default | 150 | 2,500,000 | 375,000 | 1,250,000 | 256 |
| large | 300 | 5,000,000 | 750,000 | 2,500,000 | 512 |
| max | 500 | 10,000,000 | 1,500,000 | 5,000,000 | 4,096 |

[Source caps](src/opts.rs#L18-L86). A smaller **output** does not necessarily
reduce core graph/dominator work. Other limits include 20,000 immediate-dominator
pairs, 25 big drops, 60 merged-path nodes, 10 top individual arrays/kind, 500 type
edges, capped referent/ThreadLocal/constant-array samples, and capped content maps.

Basic skips unreachable-forest dominance above
[500,000 unreachable objects](src/unreachable_retained.rs#L126-L136), retaining
shallow/composition information. CLI Full forces that pass; MCP/browser Full
currently does not. Report “skipped” rather than “no garbage cluster.”

### Cached reports

[MCP cache guidance](src/mcp/mod.rs#L318-L342) explicitly notes that the report
cache is keyed by dump path, not these analysis options. Requesting Full later
can therefore return old Basic data. Inspect `analysis_flags` and coverage;
regenerate/invalidate as appropriate rather than treating requested mode as
completed mode. New report fields/population rules must follow the `VERSION`
guidance in [CLAUDE.md](CLAUDE.md) and [src/cache.rs](src/cache.rs).

## Cost model and budgets

These are **incremental planning estimates**, not fresh benchmarks. No Rust
build, heap analysis, or performance benchmark is run on the Mac; actual Rust
RSS/runtime validation belongs on the ThinkStation. Existing comments and
[older tier notes](docs/report-size-benchmarks.md) are context, not measurements
of all current paths or of a “typical” production heap.

- `N`: objects; `E`: references; `C`: class rows; `T`: threads; `F`: observed
  stack frames; `K`: selected/reported rows; `B`: payload bytes decoded;
  `P`: top-level dominators; `G`: distinct value groups.
- Arithmetic examples use **N=10 million, E=30 million, C=20,000, T=500**.
- **ΔJSON 0 B** means reuse existing serialized data in that producer profile,
  **not** that the profile itself is free. Replotting/sorting client-side does
  not justify duplicating raw values, per-instance payloads, or derived averages.
- UI estimates are marginal **gzip JavaScript/CSS budgets**, not DOM/Rust memory.
  Shared layouts are paid once, not once per ID. Tables: ~1–4 KiB; small SVG
  layouts: ~2–8 KiB; a shared inspector/navigation: ~6–16 KiB. These ranges are
  unmeasured implementation budgets, not promised bundle deltas.
- Compressed HTML data delta is about `4/3 × deflated new JSON`, plus embedded
  bundle/wrapper changes. Compression varies; JSON integer fields include keys,
  punctuation, and decimal digits, not just Rust scalar width.
- Increasing Full/size can allocate **tens to hundreds of MiB or more** for
  captured records/maps, and can change graph lifetimes. Do not add every cap
  or historical RSS comment into a fabricated measured peak.

### Major Rust cost drivers

| Producer / feature | Source-derived incremental cost | Interpretation |
| --- | --- | --- |
| Existing chart/filter/tooltip | No extra Rust heap pass or per-object scratch | Browser O(K) or O(K log K); ΔJSON usually 0 B |
| Optional collection detail | 16 B/captured attribution edge: 40 MB at the default 2.5M cap; container records ~32 B: ~12 MB at 375K, plus maps/slots/names | Lower-bound record arithmetic, not total RSS; decoding/join work depends on entries/slots and can require rereads |
| Duplicate detection | String-address/signature maps grow with Strings/distinct backing arrays; payload decoding/hashing costs O(B) | Some hooks are fused, but scans/decoder state are not zero; capped output does not bound all input maps |
| Reference labels | About 2E bytes for labels, plus name pools | ~57 MiB at E=30M; separate `--ref-paths`, not Full |
| CLI field statistics | Saved CSR about `4(N+1)+4E` bytes, plus field counters | ~153 MiB at the example sizes, O(N+E_selected); separate `--field-stats` |
| Captured object graph | Depends on significant captured nodes/edge cap, plus core-array lifetimes | No universal “30 MB only” guarantee; bounded export is not exhaustive reachability |
| New metadata forwarding | Roughly 8–32C bytes of scratch/counters before names/maps | ~0.15–0.61 MiB at C=20K; do it before freeing class metadata |
| New global cut/SCC/union/ranking | May need bitsets/index arrays and retained adjacency | A visited bitset alone is N/8 (~1.19 MiB); queues/index arrays add 4–16N or more, and adjacency may need retention/rereading |

The producer [free-as-you-go order](src/report/build.rs#L605-L748) drops
`dc_offsets/dc_targets`, captures, and `idom` before later sections. A new view
that needs them late can cost far more than its final 5 KiB JSON. Preserve that
release order; prefer a fused bounded summary or explicit indexed query.

**Proposed display/export budgets:** first 20–25 rows, 10×10 optional heatmaps,
selected subtrees ≤200 visible nodes, at most 3 headline findings, ≤32 KiB raw
new on-demand result summaries unless justified. These are **new UI budgets**,
not a claim that existing 20K-pair or core data will be deleted or is fully covered.
All omitted/capped/unknown states remain inspectable.

## Every original idea

The linked entries provide a short verdict, pros/cons, gate, size and Rust cost
for **each** ID. “Merge” is a useful slice in an existing task, not another panel.

| ID | Reviewed slice | Decision | Default placement |
| --- | --- | --- | --- |
| [A1](#a1-dominator-sunburst) | Dominator sunburst | Contextual mode | Owners |
| [A2](#a2-who-holds-which-payload-classes) | Holder/payload pairs | Keep; table first | Owners |
| [A3](#a3-big-drop-stepdown-remainder) | Big-drop remainder | Merge | Owners |
| [A4](VISUALIZATION_IDEAS_DEFERRED.md#a4-immediate-dominator-chord-diagram) | Chord | Defer | Specialist |
| [A5](#a5-same-class-dominator-nesting) | Same-class nesting | Merge | Owners |
| [A6](VISUALIZATION_IDEAS_DEFERRED.md#a6-whole-heap-reference-cycles--scc-explorer) | Global SCCs | Defer | Specialist |
| [B1](#b1-retention-sankey) | Retention Sankey | Narrow/merge | Owners |
| [B2](#b2-shared-ownership-and-reference-cut-evidence) | Shared ownership / cut impact | Keep evidence; separate new query | Owners |
| [B3](#b3-gc-root-type-attribution) | Root-type attribution | Merge | Owners |
| [B4](#b4-direct-buffer-retained-owners) | Direct-buffer ownership | Contextual | References |
| [C1](#c1-category-budget) | Category composition | Merge | Consumers |
| [C2](#c2-framework-footprint-and-cache-evidence) | Framework/cache evidence | Narrow/merge | Consumers |
| [C3](#c3-fat-class-scatter) | Per-instance size scatter | Merge | Consumers |
| [C4](#c4-size-distribution) | Distribution | Correct/merge | Overview/Consumers |
| [C5](#c5-retention-concentration-milestones) | Concentration milestones | Merge | Overview |
| [D1](#d1-waste-waterfall--treemap) | Waste summary | Merge with J5 | Collections |
| [D2](#d2-collection-capacity-versus-usage) | Capacity/usage scatter | Contextual mode | Collections |
| [D3](#d3-string-content-and-compact-storage) | String content/encoding | Narrow/contextual | Collections |
| [D4](#d4-tinyempty-collection-overhead-by-field) | Empty/singleton field overhead | Keep | Collections |
| [D5](#d5-collection-content-types) | Collection contents | Merge into detail | Collections |
| [D6](VISUALIZATION_IDEAS_DEFERRED.md#d6-field-null-rate--object-layout-inspector) | Null-field layout | Defer | Specialist |
| [E1](#e1-thread-state-breakdown) | Thread state | Merge into E3 | Threads |
| [E2](#e2-threadlocal-stale-values) | Stale ThreadLocal values | Keep | Threads |
| [E3](#e3-per-thread-retained-memory) | Thread-object retained | Keep | Threads |
| [E4](#e4-stack-locals-depth-and-repeated-frames) | Stack locals/depth | Merge into detail | Threads |
| [E5](#e5-monitor-rooted-objects) | Monitor roots | Narrow/merge | References |
| [F1](#f1-loader-topology-and-zero-direct-instance-classes) | Loader/zero-instance scope | Contextual | Consumers |
| [F2](#f2-duplicate-loaded-classes) | Duplicate loaded classes | Merge | Consumers |
| [F3](#f3-superclasssubtype-memory-rollup) | Superclass scope | New heap-only/contextual | Consumers |
| [G1](#g1-population-comparison-and-trends) | Population trends | Keep comparison | Compare |
| [G2](VISUALIZATION_IDEAS_DEFERRED.md#g2-animated-diff-treemap) | Animated diff | Defer | Not default |
| [G3](#g3-type-edge-delta-table) | Type-edge deltas | Contextual | Compare |
| [H1](#h1-fix-roi-priority-ranker) | Fix ROI | Replace with evidence priority | Overview |
| [H2](#h2-deterministic-triage-digest) | Deterministic digest | Keep concise | Overview |
| [H3](#h3-coverage-limits-and-provenance) | Coverage/provenance | Keep | Evidence |
| [I1](#i1-softweakphantom-reference-evidence) | Reference-kind stats | Keep contextual | References |
| [I2](#i2-package-retention-treemap) | Package scope | Merge | Consumers |
| [I3](#i3-unreachable-object-composition) | Unreachable objects | Contextual | Evidence |
| [I4](VISUALIZATION_IDEAS_DEFERRED.md#i4-allocation-flame-graph) | Rare allocation traces | Defer from navigation | Trace-bearing dumps only |
| [J1](#j1-boxed-number-footprint) | Boxed objects | Contextual footprint | Collections |
| [J2](#j2-object-header-overhead) | Header overhead | Contextual estimate | Collections |
| [J3](#j3-duplicate-primitive-arrays) | Duplicate primitive arrays | Keep contextual | Collections |
| [J4](#j4-constant-primitive-arrays) | Constant arrays | Narrow/contextual | Collections |
| [J5](#j5-optimization-opportunity-overview) | Optimization overview | Keep one merged list | Collections |
| [J6](VISUALIZATION_IDEAS_DEFERRED.md#j6-duplicate-arbitrary-immutablevalue-objects) | Arbitrary value duplicates | Defer | Specialist |
| [J7](#j7-throwable-and-stored-stack-traces) | Exceptions/trace footprint | Contextual query | Collections |
| [K1](#k1-top-retainers) | Top retainers | Merge into owners | Owners |
| [K2](#k2-fields-by-retained-size) | Field attribution | Merge into owners | Owners |
| [K3](#k3-multi-mode-scatter-explorer) | Multi-scatter explorer | Simplify | Consumers/Collections |
| [K4](#k4-leak-trace-annotations) | Lifetime annotations | Replace unsafe labels | Owners |
| [K5](#k5-group-instances-by-business-field-value) | Business-value grouping | Keep guided query | Consumers |
| [L1](#l1-collection-kind-composition) | Collection kind | Merge into filter | Collections |
| [L2](#l2-map-collision--bucket-analysis) | Map collisions | Correct proxy; explicit query | Collections |
| [L3](#l3-largest-individual-arrays) | Largest arrays | Keep | Collections |
| [L4](#l4-largest-held-collections) | Largest held collections | Merge | Collections |
| [M1](VISUALIZATION_IDEAS_DEFERRED.md#m1-secret-scanner-dashboard) | Secret scanning | Separate sharing workflow | Security |
| [M2](#m2-jvm-environment-and-decoded-properties) | JVM properties | Narrow/contextual | Evidence |
| [M3](VISUALIZATION_IDEAS_DEFERRED.md#m3-hprof-record-composition-sunburst) | Record sunburst | Defer chart | Existing census |
| [N1](#n1-large-array-facts-instead-of-guessed-humongous-regions) | Humongous classification | Keep only large-array facts | Collections |
| [N2](#n2-finalizer-population-evidence) | Finalizer pressure | Narrow/contextual | References |
| [N3](#n3-heap-health-kpi-dashboard) | Heap-health KPIs | Replace with facts | Overview |
| [N4](#n4-per-thread-retained-size-chart) | Thread-retained chart | Merge duplicate E3 | Threads |
| [P1](#p1-pool-state-from-pool-fields) | Pool size/active/idle/queue | New heap-only; keep | Threads/Consumers |
| [P2](#p2-finalizer-referents-by-class) | Finalizer referents by class | New heap-only; keep | References |
| [P3](#p3-cleaner-registrations-by-resource) | Cleaner registrations | New heap-only; contextual | References |
| [P4](#p4-direct-buffer-unique-capacity) | Unique direct-buffer capacity | Correct B4; keep | References |

## User verdicts

How much each idea helps a user who asks "why is my heap big?" or "what
caused this OOM?". The **Shown when** column applies the single-report rule:
there is one report, and a view appears only when the dump provides its data.

- **Must-have:** directly names what holds memory or what to fix.
- **Helpful:** speeds up the next investigation step.
- **Nice-to-have:** occasionally useful context.
- **Skip:** adds noise or misleads most users; not shown.
- **Merged:** its useful part lives in another view.

| ID | User verdict | Shown when | Why, from the user's view |
| --- | --- | --- | --- |
| A1 | Helpful | An owner is selected | See the shape of one owner's retained memory; a whole-heap sunburst is unreadable |
| A2 | Must-have | Always | "Who is holding all these `byte[]`?" |
| A3 | Helpful | Always | Shows where retention fans out into many small children |
| A4 | Skip | Never | A2's table answers the same question more clearly |
| A5 | Helpful | Same-class nesting found | Spots linked lists, trees, and recursive caches |
| A6 | Skip | Never | Cycles are normal in Java and are not leak evidence |
| B1 | Nice-to-have | Type graph captured | Small ownership flows only; large Sankeys are unreadable |
| B2 | Must-have (evidence) / Helpful (cut size) | An object is selected | Show observed alternate referrers without an automatic caution or promised savings; exact cut impact runs on request |
| B3 | Helpful | Always | Static, thread, and JNI roots point to different fixes |
| B4 | Must-have | Direct buffers present | Shows reported buffer capacities and heap owners, not verified native-memory consumption |
| C1 | Helpful | Always | My code or a library? |
| C2 | Helpful | A framework is detected | Names the framework holding memory; cache settings only when decoded |
| C3 | Nice-to-have | Always | Finds unusually large instances; the class table usually suffices |
| C4 | Nice-to-have | Always | Context for the overall shape of the heap |
| C5 | Helpful | Always | One culprit or many? |
| D1 | Merged into J5 | — | One ranked fix list beats a second waste chart |
| D2 | Helpful | Collections pass ran | Finds oversized maps and lists and the fields holding them |
| D3 | Helpful | Duplicates pass ran | Which duplicate strings and who holds them; compact-storage savings only after encoding-correct accounting |
| D4 | Must-have | Collections pass ran | Names the field to lazy-init or share |
| D5 | Helpful | A collection is selected | What is inside the big collection |
| D6 | Skip | On request | Object-layout tuning for specialists |
| E1 | Nice-to-have | Thread states decoded | Context only; WAITING is normal for pool threads |
| E2 | Must-have | Stale entries present | Classic pool-thread leak with a clear fix |
| E3 | Must-have | Threads present | "This worker thread holds 2 GB" |
| E4 | Helpful | Stack data present | Which frame or local holds the payload; no automatic recursion verdict |
| E5 | Nice-to-have | Monitor roots present | Rare; this is not a lock analyzer |
| F1 | Helpful | More than one app loader | App servers and plugins; hidden for single-loader apps |
| F2 | Helpful | Duplicate loaded definitions found | Show separate loader definitions without claiming a redeploy leak |
| F3 | Helpful | A selected application-family base class, once implemented | Add descendant counts and shallow bytes without summing retained totals |
| G1 | Must-have | Two or more dumps; warn if captures differ | Show observed changes, not leak confirmation; two dumps are a delta, not a trend |
| G2 | Skip | Never | Animation hides what changed |
| G3 | Helpful | Two or more dumps with type graphs | Shows which holder gained the references |
| H1 | Helpful | Always | An ordering of findings by evidence, not invented ROI |
| H2 | Must-have | Always | The three things to look at first |
| H3 | Must-have | Always | Says when "nothing found" really means "not checked" |
| I1 | Helpful | References present | Explains why GC has not freed a cache |
| I2 | Helpful | Always | Package as a scope filter, not another treemap |
| I3 | Helpful | Unreachable objects present | Shows recorded unreachable composition without inferring capture phase or GC health |
| I4 | Must-have | Allocation traces present | Connects memory to allocating code; hidden for most dumps, which lack traces |
| J1 | Helpful | Boxed numbers significant | Easy fix when the volume is large |
| J2 | Nice-to-have | Always | Educational; rarely the fix |
| J3 | Helpful | Duplicates pass ran | Clear deduplication target |
| J4 | Nice-to-have | Constant arrays found | Small, specific savings |
| J5 | Must-have | Any opportunity found | One ranked list of fixes with honest, non-additive estimates |
| J6 | Nice-to-have | On request | Specialist value-object deduplication |
| J7 | Helpful | Throwables present | Shows exceptions and stack traces being kept around |
| K1 | Must-have | Always | The usual starting point of an investigation |
| K2 | Must-have | Collections pass ran | Names the exact field to change |
| K3 | Nice-to-have | Always | One scatter mode is enough |
| K4 | Helpful | Always | Measured lifetime facts; no "JDK = safe" labels |
| K5 | Must-have (multi-tenant apps) | On request | "Tenant 42 holds 70% of all sessions" |
| L1 | Nice-to-have | Collections pass ran | Works as a filter |
| L2 | Nice-to-have | Collections pass ran | Bucket occupancy; real collision chains on request |
| L3 | Must-have | Always | Concrete large allocations with their owners |
| L4 | Merged into D2/L3 | — | Same answer as the collections and array views |
| M1 | Helpful (outside the report) | On request | Check a dump before sharing it |
| M2 | Nice-to-have | Properties decoded | Context for reading the rest of the report |
| M3 | Skip | Never | Record counts are not bytes |
| N1 | Helpful (inside L3) | Always | Large-array facts without guessed G1 regions |
| N2 | Helpful | Finalizers present | Registered finalizable population; P2 names referent classes, not queue pressure |
| N3 | Helpful | Always | Measured facts instead of a single health score |
| N4 | Merged into E3 | — | Duplicate thread chart |
| P1 | Must-have | Pool classes present | Shows verified pool/queue snapshot; not a throughput or exhaustion verdict |
| P2 | Must-have | Finalizers present | Names registered finalizable classes, not pending queue members |
| P3 | Helpful | Cleaners present | Shows decoded cleanup actions; referent class only when available |
| P4 | Helpful | Direct buffers present | Adds unique-backing capacity only for supported, verified layouts |

**Totals:** 17 Must-have, 29 Helpful, 12 Nice-to-have, 5 Skip, 3 Merged (66 IDs). G4, N5, and N6 were removed because they need data that is not in the dump.

### Demo coverage

Which Must-have views have a prototype in the
[demo page](docs/visualization-demos.html), and what each still needs.

| Must-have | Demo | Status |
| --- | --- | --- |
| H2 + H3 | [Triage digest with coverage](docs/visualization-demos.html#demo-h2h3) | Matches the review: ≤3 findings, hidden views listed with reasons |
| A2 | [Who-holds-whom heatmap](docs/visualization-demos.html#demo-heatmap) | Older demo; the review prefers a ranked pair table first |
| B2 | [Blame propagation bars](docs/visualization-demos.html#demo-blame) | Shows the chain; "also referenced by" evidence still missing |
| B4 + P4 | [DirectByteBuffer owners](docs/visualization-demos.html#demo-b4) | Shows unique versus all-buffer capacity |
| E2 | [Stale ThreadLocal values](docs/visualization-demos.html#demo-e2) | Matches the review |
| E3 | [Per-thread memory](docs/visualization-demos.html#demo-e3) | Matches; state colors are context only |
| G1 | [Growth sparklines](docs/visualization-demos.html#demo-sparklines) | Wording corrected; shared scale still to be enforced |
| J5 | [Waste dashboard](docs/visualization-demos.html#demo-j5) | Labelled as overlapping estimates, not reclaimable bytes |
| K5 | [Group by field value](docs/visualization-demos.html#demo-k5) | Matches the review |
| L3 | [Large array facts](docs/visualization-demos.html#demo-n1) | Size facts only; owner column still missing |
| P1 | [Pool state](docs/visualization-demos.html#demo-pools) | Prototype includes a queued-work signal **rejected for production triage**; only its descriptive pool data/“not decoded” states remain proposed |
| P2 | [Finalizer referents by class](docs/visualization-demos.html#demo-n2) | Includes the P3 Cleaner table |
| D4, I4, K1, K2 | — | No demo yet. K1/K2 are best shown in the shared object inspector; I4 only applies to dumps with allocation traces |

## Pools and finalization

These four ideas came out of a coverage check. Current analysis detects pools
only by class name and counts finalizers only in aggregate. Everything below
is decoded from objects and fields that are already in the heap dump.

### P1. Pool state from pool fields

- **Verdict:** Keep — shows snapshot pool size and queued work; it does not prove exhaustion or throughput.
- **Analysis:** New heap-only decoder for independently verified pool/JDK layouts; current analysis has only class-name footprint ([framework scan](src/pass2/framework_scan.rs)). Potential fields, only when layout is supported:
  - `ThreadPoolExecutor`: `corePoolSize`, `maximumPoolSize`, `largestPoolSize`, worker-set size, and `workQueue` length.
  - HikariCP: `PoolEntry.state` values in the connection bag.
  - Commons Pool2: idle-deque size versus `maxTotal`.
  - Netty: allocator arena counts.
- **Pros:** "Queue holds 48,000 tasks with this owner path" is an actionable snapshot observation when queue decoding succeeds.
- **Cons:** Field names are library-version-specific; unknown versions must show "not decoded", never zero. Redacted dumps zero primitive counters, so hide decoded counters for them. Busy/idle, latency, and growth require more than a generic one-dump pool snapshot.
- **Improve:** One row per detected pool instance (name if decodable, configured size, worker count, queue length and optionally verified queue retained attribution), with a link to the queue's owner path. Mark unsupported layouts; hide when no pool class exists.
- **Triage:** No automatic queued-work retention signal (user decision). Keep the decoded pool and queue snapshot descriptive and coverage-qualified.
- **Size:** Estimated ≤8 KiB raw for up to 50 pools; ~2–4 KiB gzip table UI.
- **Rust:** O(pool instances + queue nodes walked), with a node budget per queue. Reuses the existing per-class field-offset resolution in the collection decoder; no new full-heap pass if it is fused into the existing instance scan.

### P2. Finalizer referents by class

- **Verdict:** Keep — turns "12,800 Finalizer objects" into a class breakdown without claiming they are pending or unclosed.
- **Analysis:** New heap-only grouping. Where a supported layout exposes a non-null `java.lang.ref.Finalizer` referent, classify the referent and count unresolved records separately. The current reference decoder matches only Soft/Weak/Phantom, so this needs new verified handling rather than assuming equivalent queue semantics ([reference kinds](src/pass2/fielddecode.rs#L379-L384)).
- **Pros:** Names classes with registered finalizable objects; shallow bytes of resolved referents are additive.
- **Cons:** A registered finalizable object can be perfectly alive; population is not backlog. A pending-queue length needs the `ReferenceQueue` static/linked structure decoded and verified per JDK version.
- **Improve:** Table of referent class, count, own shallow bytes and unresolved count; pending-queue count only with separate supported queue decoding. Hide when there are no Finalizer instances.
- **Triage:** Replace the current count-only `finalizer-queue-backlog` wording with "finalizable population by class"; fire on count plus a named dominant class.
- **Size:** Same cap as the other reference histograms (≤200 classes), roughly ≤12 KiB raw; ~1–2 KiB gzip UI reusing the References table.
- **Rust:** O(Finalizer instances) with bounded class counters; cost of per-JDK layout and unresolved handling must be validated before promising a fixed memory figure.

### P3. Cleaner registrations by resource

- **Verdict:** Contextual — the modern replacement for finalizers; useful when native resources (buffers, zip and socket handles) pile up.
- **Analysis:** New heap-only grouping of supported `PhantomCleanable` layouts by cleanup-action class and referent class only when still available. Cleared phantom referents must be labelled unresolved, not assigned a resource class.
- **Pros:** Shows cleanup-action registrations and, when decoded, associated resource classes.
- **Cons:** Cleaner registrations are normal for live resources; a large count is evidence, not pending-cleanup time or a leak verdict.
- **Improve:** Merge into the References view as a "Cleaner" subtype row. Hide when no `PhantomCleanable` objects exist.
- **Size:** ≤8 KiB raw; ≤1 KiB gzip.
- **Rust:** O(phantom references) during the existing reference decode; a super-chain check per class is memoized.

### P4. Direct-buffer unique capacity

- **Verdict:** Keep as a correction to B4 — today's capacity sum overcounts slices and duplicates of one allocation.
- **Analysis:** New heap-only backing-identity decoder, gated by verified JDK buffer layouts. A `cleaner`/`att` pattern may distinguish originals from views, but a non-null cleaner alone does not establish unique native backing. Verify relationships before deduplication.
- **Pros:** A verified allocation-level capacity estimate can avoid slice overcount; owner rows can show "slice of X" when attachment evidence exists.
- **Cons:** Field names vary across JDK versions; when `cleaner`/`att` cannot be resolved, keep today's sum and label it "may include slices". Redaction does not affect reference fields, but zeroes capacity values.
- **Improve:** Show "verified unique capacity" beside "sum over all buffer views" only for supported layouts; otherwise show the existing sum with an overcount warning.
- **Size:** Bounded summary scalars are small; backing-identity tracking and owner attribution add implementation-dependent state.
- **Rust:** O(buffer instances + attachment edges) for supported layouts, with bounded identity/index state; benchmark peak memory and fail closed on unknown layouts.

## Ownership and paths

### A1. Dominator sunburst

- **Verdict:** Contextual — likely helps users see a selected owner's wide/deep retained structure; not a mandatory global overview.
- **Analysis:** Basic has bounded suspect `dominator_tree`; a larger captured/queried subtree is a separate capability. Full does not imply exhaustive expansion.
- **Pros:** Shows ownership shape and supports drill-down without reading hundreds of rows.
- **Cons:** Small arcs and deep rings hide labels; summing only leaf retained sizes loses own/unshown bytes. Dominance is not direct field connectivity.
- **Improve:** Default to a table/icicle; sunburst is an optional mode with breadcrumbs, keyboard alternatives, ≤200 visible nodes, and explicit self+unshown remainder.
- **Size:** ΔJSON 0 B for the existing tree; estimated 2–8 KiB gzip shared layout UI. Do not add another copy of the tree.
- **Rust:** 0 additional passes/per-object scratch for charting. New expansion needs a separately budgeted query; preserving the full dominator-children CSR late can dominate memory cost.

### A2. Who holds which payload classes

- **Verdict:** Keep — likely useful for escaping JDK wrapper noise and finding application owners; the ranked pair table is more important than the matrix.
- **Analysis:** Basic immediate-dominator pairs; current producer emits up to 20,000 pairs with truncation metadata.
- **Pros:** Connects small holder classes with large dominated payloads; category filters and top application-owner evidence help.
- **Cons:** A global 30×30 matrix is hard to read; pair-retained weights can overlap across levels and must not sum to total occupied heap.
- **Improve:** Rank/filter first; optional 10×10 heatmap with absolute counts/qualified retained attribution, visible omitted-pair counts, and inspect actions.
- **Size:** ΔJSON 0 B; estimated 1–4 KiB gzip marginal table/heatmap controls. Cap visible cells, not silently the underlying analysis.
- **Rust:** 0 additional work beyond the existing pair producer; client sorting O(K log K). Do not run another O(N) pair scan solely for the chart.

### A3. Big-drop stepdown remainder

- **Verdict:** Merge — useful evidence for a selected ownership bottleneck, not another waterfall section.
- **Analysis:** Basic `big_drops`, currently capped at 25 rows.
- **Pros:** Identifies parents whose retention is spread beyond the largest child.
- **Cons:** The remainder includes siblings and self/unshown bytes, not memory “absorbed” by the parent or a leak by itself.
- **Improve:** Show largest-child and remainder bars beside the selected owner, with own shallow bytes separately labeled and sibling drill-down.
- **Size:** ΔJSON 0 B; estimated 1–4 KiB gzip using shared bars/detail UI.
- **Rust:** 0 additional pass/scratch; existing dominance traversal is already paid. A diagram change does not justify another heap scan.

### A5. Same-class dominator nesting

- **Verdict:** Merge — helps recognize list/tree accumulation when it explains a significant footprint.
- **Analysis:** Basic `self_referential_classes`; current output cap is 10 rows.
- **Pros:** Exposes recursively owned node populations that flat class names obscure.
- **Cons:** Nested percentage is not exact chain length, an object-reference cycle, or proof of a GC stack overflow; legitimate trees are common.
- **Improve:** Attach a “same-class nesting” fact to the owner's detail, with counts and retained scope, not a standalone chain map or automatic flattening prescription.
- **Size:** ΔJSON 0 B; estimated ≤2 KiB gzip marginal badges/expand behavior.
- **Rust:** 0 new pass/scratch for existing metrics. Exact depth/type-shape additions would need their own bounded producer, not a frontend guess.

### B1. Retention Sankey

- **Verdict:** Narrow/merge — an ownership-pair mini-view may help; a global type-flow Sankey is more likely to confuse than explain occupied bytes.
- **Analysis:** Basic supplies root/dominator attribution. Type-reference data requires CLI report graph capture and is capped at 500 edges; MCP/browser Full is not that gate.
- **Pros:** A tiny selected-owner → payload view can explain a few relationships.
- **Cons:** Type graphs can be cyclic and non-conservative. `retained_weight = edge_count × average source retained` is an estimate, not conserved bytes or cut savings.
- **Improve:** Prefer A2's table/ownership chain; if a small Sankey is used, separate actual dominance from estimated reference activity and expose scope. Never sum links as heap size.
- **Size:** ΔJSON 0 B when reusing the relevant profile; estimated 2–8 KiB gzip optional shared D3 rendering. No Nivo addition.
- **Rust:** 0 for the chart; enabling type capture adds existing graph/pair work, including an O(N) source-class weight scan. A new conserved partition is a new producer, not formatting existing weights.

### B2. Shared ownership and reference-cut evidence

- **Verdict:** Keep evidence — likely high value; do not promise an automatic exact “bytes freed” feature from capped paths.
- **Analysis:** Basic merged ownership paths support a qualified bar; alternative referrers need report/live-query graph evidence. Exact cut impact is **new heap-only**, not an existing Full result.
- **Pros:** Warns users that removing one observed field may leave another retaining route.
- **Cons:** Inbound degree is not unique lifetime ownership; cycles, direct roots, multiple fields, and missing edges matter. Object retained size is not arbitrary edge-cut savings.
- **Improve:** Show observed referrers/coverage and distinguish ownership chains from field paths. Put a complete-graph counterfactual behind an explicit read-only query with timeout/inconclusive states.
- **Size:** Basic ΔJSON 0 B; proposed new cut/ownership summary ≤16 KiB raw without full graph export; estimated 2–8 KiB gzip shared detail UI.
- **Rust:** Chart 0. A cut test costs O(N+E), at least N/8 visited bits (~1.19 MiB at example N) plus frontier/index/adjacency state, possibly 4–16N or more. Core arrays may require retention or indexed rereading; no fixed runtime/RSS guarantee.

### B3. GC-root-type attribution

- **Verdict:** Merge — useful orientation inside Owners, with root-type counts separate from dominated-byte attribution.
- **Analysis:** Basic root-type retained rows/top classes; a representative root-type assignment is not all possible root paths.
- **Pros:** Directs an investigation toward thread/static/JNI lifetimes before reading every object.
- **Cons:** A dominated object's chosen root type does not mean every path uses that type; top class lists are not a complete additive breakdown.
- **Improve:** Show one compact ranked list and click-through evidence; do not duplicate it as a Sankey, pie, treemap, and bar simultaneously.
- **Size:** ΔJSON 0 B; estimated ≤2 KiB gzip shared ranking/filter UI.
- **Rust:** 0 new pass/scratch for existing attribution. Enumerating all roots/path unions would be a separate graph query.

### B4. Direct-buffer retained owners

- **Verdict:** Contextual — valuable when direct buffers are significant; not part of the Java-heap total.
- **Analysis:** Basic capacity counters and up to 20 `nio_buffer_owners` with bounded ownership chains.
- **Pros:** Finds Java objects keeping large native-buffer wrappers reachable.
- **Cons:** Slices/duplicates can share backing storage; sum of buffer capacities is not unique committed native memory. Ownership chains are not necessarily direct references.
- **Improve:** Keep capacity and heap-retained columns separate; inspect a representative attachment/cleaner where decoded; show “reported capacity,” not “exact native bytes consumed.”
- **Size:** ΔJSON 0 B; estimated 1–4 KiB gzip contextual table/path controls.
- **Rust:** 0 new work for existing top buffers. Unique native-backing accounting requires supported in-dump attachment/capacity analysis; dump-only mode does not claim process RSS.

### K1. Top retainers

- **Verdict:** Merge — very useful entry into Owners; the existing ranked table is not a missing dataset.
- **Analysis:** Basic can contribute frame/local-derived rows; collection/field rows come from Collections. Current builder merges fields and significant frames, capped at 20.
- **Pros:** Names concrete fields/frames instead of leaving the user at a large JDK class.
- **Cons:** Field/frame attribution can overlap and does not prove an easy fix or guaranteed savings; locals are bounded samples.
- **Improve:** Use one owner ranking with evidence-kind and scope columns, not a second “unified dashboard” duplicating H1/K2.
- **Size:** ΔJSON 0 B; estimated ≤2 KiB gzip marginal ranking/links.
- **Rust:** 0 incremental chart pass/scratch; enabling collection detail has the separate profile cost above. No extra scan just to reorder the same rows.

### K2. Fields by retained size

- **Verdict:** Merge — often actionable once an owner is selected; do not add a global second heatmap.
- **Analysis:** Collections `fields_by_size`, not the separate CLI `field_stats` null-rate feature.
- **Pros:** Identifies the particular field behind a holder's footprint.
- **Cons:** Pointee-retained sums and different field groups may overlap; caps/deduplication scope must be exposed.
- **Improve:** A sortable per-owner field list with sample pointees/root evidence; display qualified attribution, not additive “field budgets.”
- **Size:** ΔJSON 0 B in the Collections profile; estimated 1–4 KiB gzip shared table controls.
- **Rust:** Chart 0; collection field-edge capture/joins and bounded pointee sets are existing optional costs. Avoid another global heatmap aggregate.

### K4. Leak-trace annotations

- **Verdict:** Replace unsafe labels — annotations help only when grounded in actual evidence.
- **Analysis:** Basic class/category/ownership information; direct field names where produced/decoded, not presumed from dominance. Expected object lifetime is not in ordinary HPROF metadata.
- **Pros:** Explains unfamiliar wrappers and missing field/path evidence.
- **Cons:** Coloring JDK/framework objects “known alive/safe” and application objects “likely leak” is incorrect and biases the investigation.
- **Improve:** Neutral category badges, measured retention/drop facts, real field labels and unknowns. Put lifecycle/dead-object assertions in the deferred external-evidence slice.
- **Size:** ΔJSON 0 B; estimated ≤2 KiB gzip neutral annotations.
- **Rust:** 0 new pass/scratch for existing facts. A lifecycle registry or synthetic outer-reference scan is additional work and not justified as a universal low-cost annotation.

## Consumers and distributions

### C1. Category budget

- **Verdict:** Merge — useful first scope control, but code category is not lifetime responsibility.
- **Analysis:** Basic category rollup; current viewer already renders category data.
- **Pros:** Lets users scope application/framework/JDK class populations without knowing names.
- **Cons:** JDK objects can be retained by application caches; category-retained totals are not an additive heap partition.
- **Improve:** Default to additive shallow composition, a visible category filter, and owner drill-down. Do not add another category treemap section.
- **Size:** ΔJSON 0 B; estimated ≤2 KiB gzip shared scope/chip UI.
- **Rust:** 0 additional pass/scratch; classify/rollup already happens in the core producer. User-defined categories remain an explicit input.

### C2. Framework footprint and cache evidence

- **Verdict:** Narrow/merge — useful context; a framework bar alone does not identify the application retaining it.
- **Analysis:** Basic detected-framework results already have a viewer table. Versioned per-cache literal policy decoding is **new heap-only**, not automatically provided by Full.
- **Pros:** Reveals heavy framework populations and, with supported decoding, actual cache size/policy fields.
- **Cons:** Sentinel/name heuristics can miss frameworks; footprint is not proof of faulty eviction. Snapshot occupancy cannot prove TTL effectiveness, activity, or hit rate.
- **Improve:** Reuse Consumers scope and owner links. Add only supported, evidence-sourced cache fields in selected detail; unknown policy ≠ no policy. Defer operational eviction claims.
- **Size:** Existing ΔJSON 0 B; proposed ≤16 KiB raw for 20 bounded cache-policy rows; estimated 2–6 KiB gzip shared detail UI.
- **Rust:** Existing chart 0. New adapters require bounded instance/field decoding, potentially O(N+B_selected) scans or indexed queries plus O(K) output; no single universal library layout or seconds estimate.

### C3. Fat-class scatter

- **Verdict:** Merge — sometimes useful to distinguish few large instances from many small ones; a table is the default.
- **Analysis:** Basic `histogram.shallow / instances`; **no new average field is necessary**. Guard zero-instance rows and label arrays' variable sizes.
- **Pros:** Surfaces instance-count/average-size outliers quickly.
- **Cons:** Means hide distributions, logarithmic axes confuse, and large average size is not waste.
- **Improve:** A Consumers scatter mode with exact table fallback and shared scales; click a point to inspect large instances/owners. Do not also create a separate K3 screen.
- **Size:** ΔJSON 0 B; estimated 2–6 KiB gzip marginal scatter wiring, using existing chart infrastructure.
- **Rust:** 0 new pass/scratch/divisions in Rust; client O(C) derived values. Serializing extra averages would unnecessarily increase every histogram row.

### C4. Size distribution

- **Verdict:** Correct/merge — useful, provided axes and populations are truthful.
- **Analysis:** Basic `TopSizeDistribution` is **top-level dominator retained sizes**, not every object's shallow size. `ArraysBySize` is array **element length**, not bytes.
- **Pros:** Distinguishes diffuse ownership from a few large top-level regions; array lengths provide separate context.
- **Cons:** Existing ideas conflate retained bytes, shallow bytes, and array lengths. A single maximum per class cannot identify every object in a bucket.
- **Improve:** Label “Top-level ownership regions by retained bytes” and keep an independent array-length/table view; an all-object shallow histogram would be a new fused producer.
- **Size:** Existing ΔJSON 0 B; estimated 1–4 KiB gzip histogram controls. Proposed new 32-bucket shallow histogram ≤4 KiB raw.
- **Rust:** Existing chart 0. New shallow-size bins can be O(N), O(32) counters if fused into the existing size loop; do not add a separate late scan or retain size arrays for UI convenience.

### C5. Retention concentration milestones

- **Verdict:** Merge — “top 1/10/100 ownership regions” usually explains concentration better than a global Lorenz chart.
- **Analysis:** Basic exact top-1/10/100 bytes and shares over top-level dominators.
- **Pros:** Tells users whether to investigate a few owners or repeated small-object overhead.
- **Cons:** Three points do not encode a full distribution, arbitrary 80% rank, or a Gini/Lorenz curve; nested object/class retained sums are invalid substitutes.
- **Improve:** Keep exact discrete milestones with links to ranked owners. Defer a smooth/full Lorenz view unless a real task justifies additional global ranking.
- **Size:** ΔJSON 0 B; estimated ≤2 KiB gzip milestone/selection UI. Optional full bounded curve could be ≤8 KiB raw, but is not required.
- **Rust:** Existing milestones 0 new work. Exact full ranking can require O(P log P) time and an 8P size array (~76 MiB at P=10M), in addition to normal producer data; bounded approximation must be labeled.

### I2. Package retention treemap

- **Verdict:** Merge — package navigation is useful, but the existing package view is not missing.
- **Analysis:** Basic package tree/current Consumers rendering; histogram can derive additive shallow package composition.
- **Pros:** Finds application areas when the user knows package prefixes but not classes.
- **Cons:** A package name is not a verified module/JAR or retaining owner; retained attribution can overlap and pruning hides small classes.
- **Improve:** Make Package a Consumers scope/view mode, preserving current table/treemap data. Use shallow totals for an additive composition mode and retained values only with explicit semantics.
- **Size:** ΔJSON 0 B; estimated ≤2 KiB gzip shared scope/drill-down controls.
- **Rust:** 0 new pass/scratch; client derivation over the existing tree/histogram. Do not duplicate package data to support another treemap component.

### K3. Multi-mode scatter explorer

- **Verdict:** Simplify — two contextual scatter modes can help; a four-mode standalone explorer duplicates several destinations.
- **Analysis:** Basic class metrics; Collections adds container/field samples. Aggregate map-occupancy buckets cannot be joined to individual Map points as collision evidence.
- **Pros:** Makes selected-class or selected-container outliers easy to compare.
- **Cons:** Overloaded axes/point sizes are hard to learn; a log plot's distance is a ratio, not `retained-shallow` or savings. Four tabs/lasso/batch sums amplify overlap confusion.
- **Improve:** Class mode in Consumers, capacity/usage mode in Collections; one selected row and exact table fallback. Defer lasso and invented collision overlays until user tests justify them.
- **Size:** ΔJSON 0 B for existing rows; estimated 2–8 KiB gzip shared scatter/selection UI, paid once rather than for four new sections.
- **Rust:** 0 additional chart work; optional producer costs apply only when enabled. Correct per-map collision data is a new explicit query, not a frontend bucket lookup.

### K5. Group instances by business-field value

- **Verdict:** Keep guided query — likely high value for “which tenant/status/key consumes the memory?”
- **Analysis:** Existing supported OQL `GROUP BY`/field decoding; not part of default report. The guided selector is new UI; exact retained-set unions are a separate new query.
- **Pros:** Converts object populations into recognizable business buckets without a second query language.
- **Cons:** High-cardinality groups consume memory; reference identity differs from decoded value; unresolved/redacted fields cannot be treated as real keys. Group retained sums can overlap.
- **Improve:** Choose a supported field, count/add shallow bytes, mask sensitive values, show coverage, and inspect representatives. Make “retained union” explicit and avoid additive charts across overlapping unions.
- **Size:** Existing requested query results need no duplicated schema; proposed displayed/exported result ≤32 KiB raw for 100 masked rows; estimated 2–8 KiB gzip guided-query UI.
- **Rust:** Existing query cost depends on selected rows/decoding, O(S+B_selected), plus O(G) keys/counters and possible seed arrays. Propose a 16–64 MiB group/work budget with partial results. A retained union can require O(N+E) graph work and additional bitsets/frontiers; it is not Basic/Full output already.

## Collections and optimization opportunities

### D1. Waste waterfall / treemap

- **Verdict:** Merge with J5 — a ranked opportunity list helps; another additive waste chart does not.
- **Analysis:** Basic has available capacity/array estimates; Duplicates and Collections add sources. Current `WasteSummary` estimates may overlap and mix reachable/unreachable input scope.
- **Pros:** Gives a short path from a large optimization estimate to relevant evidence.
- **Cons:** The summed existing total is not a verified net reclaimable amount; a proportional reachability adjustment is not per-object live accounting.
- **Improve:** Show non-additive rows, source/coverage/estimate labels, and evidence links. Do not headline “X bytes reclaimable with no functional changes.”
- **Size:** ΔJSON 0 B; estimated ≤2 KiB gzip shared opportunity-list UI.
- **Rust:** 0 new chart work. Actual net/live savings would require deduplicated object-level accounting, not another sum of existing category totals.

### D2. Collection capacity versus usage

- **Verdict:** Contextual mode — useful after filtering to a container kind/owner; table first.
- **Analysis:** Basic aggregate fill/size evidence; selected per-container capacity/owner detail primarily comes from Collections. Verify which row shape actually has capacity.
- **Pros:** Distinguishes sparse backing storage from large but densely used collections.
- **Cons:** `BiggestCollectionRow` does not magically contain every capacity/waste metric; retained payload size is not empty-slot overhead, and capacity has type-specific meaning.
- **Improve:** Join verified same-object detail where available; otherwise mark missing. Compare elements/capacity in slots and estimated unused backing bytes separately, with an exact row fallback.
- **Size:** ΔJSON 0 B when the detail exists; estimated 2–6 KiB gzip using K3's shared scatter/table UI.
- **Rust:** Chart 0; enabling Collections pays capture/join costs. A new missing per-instance metric needs a bounded producer, not guessed values from aggregate histogram buckets.

### D3. String content and compact storage

- **Verdict:** Contextual — content groups may orient users, but the current duplicate samples cannot describe the entire String universe.
- **Analysis:** Duplicates supplies 25 top duplicate samples (text capped at 200 bytes) and length/holder data. Encoding totals are **new heap-only**; the decoder already handles coder/Latin1/UTF-16.
- **Pros:** Connects large repeated strings to likely subsystems and can expose genuinely Latin1-compatible wide storage.
- **Cons:** Truncated samples can misclassify URLs/JSON; UTF-16 for non-Latin1 content is legitimate. Current slack logic compares backing bytes with UTF-8 decoded length, so it is not a trustworthy physical String-slack metric across encodings.
- **Improve:** Call sample categories “top duplicate samples.” Mask text by default. Fix/verify encoding-aware physical storage accounting before presenting compact/slack savings; redaction disables content/coder-based inferences.
- **Size:** Sample classification ΔJSON 0 B; proposed encoding summary ≤24 KiB raw; estimated 2–6 KiB gzip selected String detail UI.
- **Rust:** Chart 0 in a Duplicates report. A fused encoding tally is cheap, but content-eligibility/unique backing accounting costs O(B) decoding and address dedup state. Existing duplicate maps can grow with all Strings/backing arrays; impose an explicit budget rather than claiming negligible RSS.

### D4. Tiny/empty collection overhead by field

- **Verdict:** Keep — one of the most actionable heap-only details when repeated structural overhead is significant.
- **Analysis:** Collections `tiny_overhead`; Basic empty-collection/kind summaries alone cannot identify every owner field.
- **Pros:** Names a concrete holder field and repeated empty/singleton populations.
- **Cons:** Lazy-init/null/immutable replacements can change mutability, identity, API, and concurrency semantics; empty now does not mean never used.
- **Improve:** Show overhead estimate/count/owner evidence and a conditional pattern suggestion, not a promised mechanical one-line safe fix. Merge with the selected owner and opportunity list.
- **Size:** ΔJSON 0 B in Collections; estimated 1–4 KiB gzip shared table/detail UI.
- **Rust:** 0 new view work; optional attribution capture/joins already paid. Better reachability-aware unique overhead requires additional accounting, not multiplying every holder row by retained bytes.

### D5. Collection content types

- **Verdict:** Merge into selected detail — useful “what is inside this collection?” evidence; a global matrix is niche.
- **Analysis:** Collections content/type samples; globally aggregated type rows are not automatically a complete per-instance inventory.
- **Pros:** Explains unknown payload wrappers and heterogeneous caches.
- **Cons:** Sampling/caps bias counts, and a wrapper type is not always a decoded key/value type. Content-type share does not measure deep bytes.
- **Improve:** A small labeled sampled-type list on the selected container with supported key/value separation; use K5 only for genuinely decoded business values.
- **Size:** ΔJSON 0 B; estimated ≤2 KiB gzip selected-detail UI.
- **Rust:** 0 additional view work; sampling maps/slots are existing optional producer costs. Full does not make capped samples exhaustive.

### J1. Boxed-number footprint

- **Verdict:** Contextual — helps explain many-small-object overhead, not a universal waste verdict.
- **Analysis:** Basic boxed-number rows; holder ranking is Collections-dependent.
- **Pros:** Reveals wrapper populations that primitive-specialized data structures might reduce.
- **Cons:** Nullability, generic APIs, cache sharing, and semantics can require wrappers. Wrapper shallow bytes are not all reclaimable; field/layout replacement savings vary.
- **Improve:** Label observed wrapper footprint, link top owners, and offer conditional primitive-container options. No separate default boxed pie.
- **Size:** ΔJSON 0 B; estimated ≤2 KiB gzip marginal opportunity/detail UI.
- **Rust:** 0 new pass/scratch for existing rows. Enabling owner attribution has the collection profile cost, not a new boxed-specific global scan.

### J2. Object-header overhead

- **Verdict:** Contextual estimate — useful for diffuse object-heavy heaps, not a top-level “waste” total.
- **Analysis:** Basic header-overhead estimates under the analyzer's size/layout assumptions.
- **Pros:** Explains why many tiny objects cost more than their logical payload.
- **Cons:** Headers are mandatory; oop/header/alignment assumptions are not universally known from HPROF ID width. Byte savings depend on a feasible representation change.
- **Improve:** Show estimated structural overhead and assumptions beside object-count evidence; avoid Valhalla/Lilliput promises or adding header bytes to duplicate savings.
- **Size:** ΔJSON 0 B; estimated ≤2 KiB gzip shared fact/detail UI.
- **Rust:** 0 extra view work. A target-JVM layout simulation would need separately verified external configuration and is outside the heap-only core.

### J3. Duplicate primitive arrays

- **Verdict:** Keep contextual — high-value evidence for repeated buffers/configuration payloads.
- **Analysis:** Duplicates; richer array holder attribution can depend on Collections plus Duplicates.
- **Pros:** Finds large duplicate byte/int/char payloads not visible in class totals.
- **Cons:** Current fingerprints/statistics are approximate; identical mutable arrays cannot automatically be shared, and String backing arrays may overlap other estimates.
- **Improve:** Show verification/coverage, representative owners, and conditional own-storage opportunities; mask content and disable value comparisons on redacted input.
- **Size:** ΔJSON 0 B in a duplicate-bearing report; estimated 1–4 KiB gzip shared duplicate/detail UI.
- **Rust:** Chart 0. Producer hashing/verification is O(B) and signature-state-dependent; capped output is not a universal small-RSS guarantee. Reuse existing scan hooks.

### J4. Constant primitive arrays

- **Verdict:** Narrow/contextual — can explain repeated structural storage, but constants are not automatically dispensable.
- **Analysis:** Basic constant-array groups and sampled owners, with coverage caps. Redacted payloads are not valid constant-array evidence.
- **Pros:** Exposes large all-zero/all-same populations and representative holder patterns.
- **Cons:** Fresh mutable buffers, cryptographic zeroing, sentinels, and later writes can be intentional; a shared immutable replacement may change semantics.
- **Improve:** Treat as observed footprint with sampled owner evidence; only suggest sharing after immutability/use is established. No automatic reclaimable category sum.
- **Size:** ΔJSON 0 B; estimated ≤2 KiB gzip shared table/detail UI.
- **Rust:** 0 for the view. Existing payload checks/group maps/sample addresses are already Basic cost; new exact owner unions would need a separate budget.

### J5. Optimization opportunity overview

- **Verdict:** Keep one merged list — likely helps users choose the next investigation; do not build a second dashboard beside D1.
- **Analysis:** Available Basic sources plus optional Collections/Duplicates; absence/partial coverage visible per source.
- **Pros:** Connects large estimates to concrete owners, arrays, duplicate groups, and structural facts.
- **Cons:** Headers, wrappers, duplicates, constant arrays, and empty capacity can refer to the same objects. Current totals are not exact net savings or guaranteed safe fixes.
- **Improve:** Sort qualified rows by impact, show top three plus “More opportunities,” separate observed footprint from opportunity estimates, and expose “not additive” instead of a grand reclaimed percentage.
- **Size:** ΔJSON 0 B for existing summaries; proposed coverage metadata is H3's shared budget, not one copy/category. Estimated 1–4 KiB gzip list/detail UI.
- **Rust:** 0 new chart pass. Deduplicated net/live savings is new object-level accounting, and should not be quietly added to Basic for a prettier total.

### J7. Throwable and stored stack traces

- **Verdict:** Contextual query — useful for error queues/cached failures, but not a routine new dashboard.
- **Analysis:** Basic histogram and existing exception-name triage; real `INSTANCEOF Throwable` queries can improve the census. Trace/cause attribution is **new heap-only**, not current Full output.
- **Pros:** Finds error objects and materialized trace storage retained by application owners.
- **Cons:** Name suffixes are not verified Throwable subtypes; retained counts are not throw frequency/CPU cost. Native/unmaterialized backtraces and shared cause/trace objects complicate accounting.
- **Improve:** Reuse selected-class query/owner detail, validate subtype membership, mask messages, and deduplicate decoded trace objects. Do not call all diagnostic storage waste.
- **Size:** Query-backed bounded census/detail proposed ≤24 KiB raw; estimated 2–6 KiB gzip reused table/query UI.
- **Rust:** Existing display 0; new decoding costs O(S+B_trace) plus visited/cause/group state, with cycle/depth/byte budgets. Actual throwing frequency requires external profiling and is deferred.

### L1. Collection kind composition

- **Verdict:** Merge into a filter — useful orientation with no justification for another treemap.
- **Analysis:** Basic kind counts/elements/collection-instance shallow bytes; current viewer already renders collection data.
- **Pros:** Shows whether the application is Map/List/queue-heavy and which kind has large instances.
- **Cons:** Instance shallow bytes exclude backing/payload storage; summing elements across different container kinds can count reused elements more than once.
- **Improve:** Kind chips and a small fact row; explicit “container-instance shallow” versus retained/backing metrics. Share D2/L4 selection.
- **Size:** ΔJSON 0 B; estimated ≤2 KiB gzip shared filters.
- **Rust:** 0 new pass/scratch for existing kind stats; no new per-element export.

### L2. Map collision / bucket analysis

- **Verdict:** Correct the proxy first — actual per-map chain evidence can help, but the current statistic is not collision quality.
- **Analysis:** Basic `map_collision_ratio` is populated with **nonempty table buckets / table length**, not entry collision-chain lengths. Selected true chains require a new complete/budgeted query, not merely Full's captured rows.
- **Pros:** Occupancy can provide capacity context; real decoded chains can reveal extreme key/bucket skew.
- **Cons:** Occupancy is neither `entries/capacity` load factor nor a collision rate. Aggregate buckets cannot color individual maps; no valid “collision wasted bytes” formula follows from table length or key class.
- **Improve:** Relabel existing data “Occupied table buckets.” For selected maps, optionally decode chain/tree bins with completeness evidence; remove “known-bad hashCode” prescriptions and guaranteed CPU/waste claims.
- **Size:** Existing ΔJSON 0 B; proposed ≤16 KiB raw for 20 verified chain summaries without automatic deep paths; estimated 2–6 KiB gzip detail UI.
- **Rust:** Existing chart 0. New indexed bin walks cost O(entries visited) with cycle/visited state; cap work/memory and report incomplete bins. Output limits alone do not bound enormous chains.

### L3. Largest individual arrays

- **Verdict:** Keep — directly helps explain large heap payloads and their owners.
- **Analysis:** Basic top primitive/object arrays, currently top 10 individual arrays per kind; owner information is best-effort and can improve with Collections.
- **Pros:** Names concrete large allocations, length, shallow bytes, and observed holder evidence.
- **Cons:** Top ten is not the full population; object-array non-null slots are not deep payload bytes. One observed holder is not the sole owner.
- **Improve:** Ranked table with byte/length units, coverage, and an owner/detail action. N1's generic size insight belongs here; no guessed G1 classification.
- **Size:** ΔJSON 0 B; estimated 1–4 KiB gzip shared table/detail UI.
- **Rust:** 0 additional chart work. Existing bounded top-array tracking is already paid; exact population bins can be fused as described in C4 rather than exporting all arrays.

### L4. Largest held collections

- **Verdict:** Merge — useful selected-container detail, redundant as a second largest-collections section.
- **Analysis:** Collections `biggest_single`/related attribution rows; verify capacity and object identity per source rather than concatenating incompatible tables.
- **Pros:** Links large container instances to a holder field with elements/capacity/retained context.
- **Cons:** Owner samples and multiple holder rows can describe the same container; retained payload ≠ capacity waste.
- **Improve:** Deduplicate selection by object when identity exists, combine with D2/D4, and preserve unknown owner/metrics. One table and one drawer.
- **Size:** ΔJSON 0 B; estimated ≤2 KiB gzip marginal table/selection integration.
- **Rust:** 0 extra view pass. New exact per-container identity/unique owner joins need bounded producer work, not another full duplicate table export.

### N1. Large-array facts instead of guessed humongous regions

- **Verdict:** Merge size facts into L3; actual G1 tuning/classification is deferred because collector/region context is not generally in HPROF.
- **Analysis:** Basic measured top-array shallow/length data. Full cannot supply missing `G1HeapRegionSize`, heap bounds, or collector telemetry.
- **Pros:** Large arrays are concrete and inspectable, independent of GC algorithm.
- **Cons:** `ArraysBySize` buckets are lengths; a fixed 2/4 MiB threshold does not establish actual G1 humongous classification, region waste, fragmentation, or GC frequency.
- **Improve:** Neutral “large array” evidence with actual bytes and owner links; remove guessed-region KPIs and collector-setting advice from the demo/report.
- **Size:** ΔJSON 0 B; estimated ≤2 KiB gzip merged labels/filtering. Verified GC-region integration is separately deferred.
- **Rust:** 0 new work for the existing samples. A new all-array byte histogram is C4's fused O(N) summary; no special Full pass can recover external region state.

### N2. Finalizer population evidence

- **Verdict:** Narrow/contextual — useful on legacy/finalization-heavy heaps, but registry count is not queue pressure proof.
- **Analysis:** Basic Finalizer histogram/current triage. A correctly decoded pending queue must come from heap structures that are actually present and decoded in the dump.
- **Pros:** Directs users toward finalization-heavy retained populations/owners.
- **Cons:** Live registered objects can legitimately await future finalization; `0x08` is a thread root, not a pending-finalizer count. Finalizing methods are not enumerated by current class metadata.
- **Improve:** Label the observed population, inspect owners/available Finalizer-rooted objects, and leave pending/stuck status unknown unless verified from dump data.
- **Size:** Basic ΔJSON 0 B; a verified bounded queue summary could be ≤8 KiB raw; estimated ≤2 KiB gzip contextual detail UI.
- **Rust:** Existing display 0. New queue-layout traversal depends on queue length, with visited/decode limits; do not claim it is one O(1) registry-size read or automatically part of Full.

## Threads and references

### E1. Thread state breakdown

- **Verdict:** Merge into E3 — useful context, weak as a standalone issue visualization.
- **Analysis:** Basic decoded state and thread-object retained fields; missing/redacted primitive states remain unknown.
- **Pros:** Filters threads by activity state without losing retention ranking.
- **Cons:** Idle/parked worker pools are normal. State-retained sums are not proof of a ThreadLocal leak, blocked-memory trap, or complete heap partition.
- **Improve:** Neutral state chips/counts, actual labels and selected-thread rows; no red WAITING treemap or inferred contention diagnosis.
- **Size:** ΔJSON 0 B; estimated ≤2 KiB gzip shared thread filters.
- **Rust:** 0 new pass/scratch; client O(T) state grouping. Thread-object retained values come from dominance, not local-sample sums.

### E2. ThreadLocal stale values

- **Verdict:** Keep — likely very helpful when stale entries retain significant values.
- **Analysis:** Basic null-key scalar; Duplicates gates value-class `thread_local_analysis`. `find_duplicates` is already recorded, but caps/provenance still matter.
- **Pros:** Gives the value class and stale-key counts needed to inspect pool-retention patterns.
- **Cons:** Stale means key referent cleared, not owning thread dead. Values can be shared; row retained totals include non-stale entries and can overlap. Some stale entries are temporary until the next cleanup.
- **Improve:** Show count evidence first, label total-versus-stale bytes accurately, inspect representative entries/owners, and propose unique stale-value accounting before assigning savings.
- **Size:** Existing ΔJSON 0 B; proposed stale-byte/coverage refinements ≤8 KiB raw; estimated 1–4 KiB gzip shared detail UI.
- **Rust:** View 0. Additional unique stale-value accounting is O(captured entries), currently capped at 500K, with bounded dedup state (potentially tens of MiB at the cap). Do not sum all row-retained bytes as stale reclaimable storage.

### E3. Per-thread retained memory

- **Verdict:** Keep — directly identifies individual thread objects that dominate significant heap.
- **Analysis:** Basic resolved thread object's `g.retained`; names/states may be missing, and dominant local class is sampled.
- **Pros:** Makes a large retained thread easy to select and connect to its local/field evidence.
- **Cons:** Retention through a Thread can come from its fields, context loader, or synthetic frame roots; it does not prove all bytes are ThreadLocal values. Thread retained sets need not form an additive partition.
- **Improve:** One ranked thread table/bar mode with stable selection, separate sampled-local metrics, and neutral state coloring; merge E1/E4/N4.
- **Size:** ΔJSON 0 B; estimated 1–4 KiB gzip shared ranking/detail UI.
- **Rust:** 0 additional pass/scratch; client O(T log T) ranking. Do not recompute per-thread retained unions solely for a chart already backed by thread-object dominance.

### E4. Stack locals, depth, and repeated frames

- **Verdict:** Merge into selected thread — locals are valuable; global recursion scoring is secondary.
- **Analysis:** Basic available stack frames and detail-capped local/frame samples; no current separate `--thread-locals` boolean is needed.
- **Pros:** Answers which observed frame/local holds a payload and collapses repetitive stacks.
- **Cons:** Samples and absent/truncated stacks are incomplete; frame count is not native stack bytes or remaining `-Xss`. Repeated frames do not prove runaway recursion.
- **Improve:** “Observed depth” and expandable repeat groups beside local evidence; no automatic recursion/stack-overflow verdict or summed overlapping local-retained totals.
- **Size:** ΔJSON 0 B for client repeat summaries; estimated 2–6 KiB gzip shared expandable-stack UI.
- **Rust:** 0 new heap pass/scratch; client O(F) summaries with bounded pattern matching. If serialized, extra per-thread summary fields are unnecessary duplication unless another consumer needs them.

### E5. Monitor-rooted objects

- **Verdict:** Narrow/merge into B3/I1 — root evidence can help occasionally; lock diagnostics are outside the heap-only report.
- **Analysis:** Basic parsed root tags/counts; specific monitor-object memberships need new bounded forwarding before representative root-type reduction. Full does not supply owners/waiters.
- **Pros:** Surfaces an unusual root lifetime when a monitor-rooted object retains a large graph.
- **Cons:** `ROOT_MONITOR_USED` supplies an ID, not monitor ownership, waiter lists, contention duration, or deadlock proof.
- **Improve:** Root-type detail/selected-object evidence only; defer synchronized thread/JVMTI lock correlations. Preserve all relevant memberships instead of assuming the representative root tag is exhaustive.
- **Size:** Existing counts ΔJSON 0 B; proposed root-object summary ≤16 KiB raw without automatic full paths; estimated ≤2 KiB gzip contextual table UI.
- **Rust:** New forwarding O(root records), with bounded O(K) output/top selection; not a default O(N+E) lock graph. External correlation costs are separately deferred.

### I1. Soft/weak/phantom reference evidence

- **Verdict:** Keep contextual — valuable when reference lifetimes explain otherwise surprising retention.
- **Analysis:** Basic reference statistics/only-weakly-retained rollups; existing viewer has a References section. Referent capture/histograms are capped.
- **Pros:** Distinguishes strong retention from observed reference-wrapper/referent states.
- **Cons:** A cleared reference is normal, not a postmortem leak. Weak/phantom semantics differ from soft caches; “no strong dominator” is an analyzer-policy observation, not guaranteed survival until a future GC.
- **Improve:** One kind selector plus counts, qualified referent footprint, coverage, and inspection. Do not create three unconditionally visible heatmaps or claim weak referents survive by luck.
- **Size:** ΔJSON 0 B; estimated 1–4 KiB gzip shared reference/detail UI.
- **Rust:** 0 new view work; current producer already pays bounded referent tracking/rollup. Full does not automatically turn capped referent observations into all-root reachability proof.

### N4. Per-thread retained-size chart

- **Verdict:** Merge duplicate E3 — useful capability, no reason for a second GC Insights implementation.
- **Analysis:** Same Basic thread fields as E3.
- **Pros:** Provides a contextual link from a finding to a retaining thread.
- **Cons:** Duplicated charts/state/denominators fragment the user's investigation.
- **Improve:** A deep link into the one Threads view, preserving selected thread and any known issue evidence.
- **Size:** ΔJSON 0 B; estimated <1 KiB gzip marginal navigation, not another chart bundle.
- **Rust:** 0 extra pass/scratch/runtime; reuse E3's existing result.

## Loaders, comparison, and evidence

### F1. Loader topology and zero-direct-instance classes

- **Verdict:** Contextual scope — zero-instance filtering is cheap; parent topology is new but can be heap-only when safely decoded.
- **Analysis:** Basic loader/histogram rows, including zero-instance classes. Parent loader ID is not in current rollup; add a bounded decoder/query before drawing a tree.
- **Pros:** Helps plugin/redeploy users compare loader-local populations and class definitions.
- **Cons:** Zero direct instances ≠ unused; utility/abstract/reflective classes can be live. HPROF does not quantify metaspace or verify JAR identity. Raw loader addresses are snapshot-local.
- **Improve:** A loader scope/filter within Consumers; show “loaded, no direct instances” and unknown parent evidence. Defer native/JAR/unused-dependency claims.
- **Size:** Filter ΔJSON 0 B; parent links roughly 20–40 B/loader in JSON before names; proposed bounded detail ≤16 KiB raw; estimated 2–6 KiB gzip scope/tree UI.
- **Rust:** Filter 0; new parent resolution O(selected loader records/decodes), O(loaders) state, potentially a payload scan if indexed fields are unavailable. Forward it before class metadata is freed; not an automatic Full feature.

### F2. Duplicate loaded classes

- **Verdict:** Merge into loader detail — useful in plugin/redeploy cases, not a universal classloader-leak verdict.
- **Analysis:** Basic duplicate-name/per-loader rows; current viewer already renders them.
- **Pros:** Reveals multiple definitions and their observed loader-local footprints.
- **Cons:** Multiple legitimate loaders can load the same name; duplicates do not prove an old loader should have died. Name/address matching across dumps is not stable deployment identity.
- **Improve:** Rank definitions with loader scope and root/owner evidence; say “multiple definitions,” not “clash” or “classic redeploy leak” without lifecycle evidence.
- **Size:** ΔJSON 0 B; estimated ≤2 KiB gzip shared loader/detail UI.
- **Rust:** 0 incremental view work; additional verified loader lifetime/deployment history is external and deferred.

### F3. Superclass/subtype memory rollup

- **Verdict:** New heap-only/contextual — likely useful when many small concrete subclasses hide a large application family.
- **Analysis:** `ClassInfo.super_id` is parsed but not forwarded as a report hierarchy. Current Full does not implement this rollup; preserve loader-distinct definitions. Interface/JAR data is separate.
- **Pros:** Lets the user scope a base application type and inspect its concrete payload population.
- **Cons:** Inclusive parent/child totals are nested; class-retained totals cannot simply be added. Generic `Object`/framework base rollups often add little information.
- **Improve:** Start with additive direct/descendant counts and shallow bytes; selected application-family scope, not another global tree. Exact retained unions are explicit queries.
- **Size:** Proposed links at ~20–40 B/class: ~0.4–0.8 MB raw for C=20K before extra names; avoid duplicating names. Estimated 2–6 KiB gzip shared hierarchy/scope UI.
- **Rust:** Forwarding/counters O(C), roughly 8–32C B before names/maps (~0.15–0.61 MiB example); aggregate postorder instead of repeatedly walking all ancestors. Object tallies can be fused; avoid prolonging all class/graph arrays.

### G1. Population comparison and trends

- **Verdict:** Keep for Compare — much stronger evidence than a single large snapshot, but not leak confirmation or an OOM forecast.
- **Analysis:** Multi-report class/count/retained arrays; Basic reports suffice. Shallow/count comparisons should be preferred for additive occupancy; header timestamps exist but need aligned series forwarding for elapsed-time rates.
- **Pros:** Separates persistent population growth from a spike, plateau, or reclamation.
- **Cons:** Name-based joins can combine loaders; capture phase/workload differ; per-row autoscaled sparklines exaggerate tiny changes. Two points always fit a line and cannot establish growth confidence.
- **Improve:** Signed deltas plus common-scale sparklines and sample counts; show observed points, time provenance, and comparability warnings. Defer workload-normalized leak proof and time-to-OOM.
- **Size:** ΔJSON 0 B for existing series; timestamp metadata about 20–40 B/capture, not just an 8-byte scalar. Any new shallow series scales O(CR); show/export a bounded selected slice. Estimated 2–6 KiB gzip comparison UI.
- **Rust:** Existing plot 0; a requested class join is O(total report rows), not a heap rescan. Timestamp forwarding O(R), fit/classification O(CR) if computed in Rust; no new per-object scratch.

### G3. Type-edge delta table

- **Verdict:** Contextual — helps a specialist explain relationship change, not the default growth or byte-budget metric.
- **Analysis:** Multi-report, with type-reference capture on both reports; usually CLI report graph/custom query capability. Full alone is not equivalent across entry points.
- **Pros:** Highlights observed new/growing class relationships for a selected owner.
- **Cons:** Top-500 ranking can make an edge “new” merely because it entered the cap; retained weights are estimated/non-additive and input profiles may differ.
- **Improve:** Display capture settings/unknown coverage and observed edge counts; label missing as “not reported” unless absence is verified. Keep under selected comparison detail.
- **Size:** ΔJSON 0 B to chart an existing diff; new bounded 100-row edge summary ~20–40 KiB raw depending on names; estimated 1–4 KiB gzip reused table UI.
- **Rust:** Join over bounded type-edge rows, not every heap edge, O(K log K) typical map/sort work with O(K) state. Input graph capture costs are separately paid.

### H1. Fix ROI priority ranker

- **Verdict:** Replace — evidence/impact ranking will help; guessing fix difficulty from root type will mislead.
- **Analysis:** Basic findings/suspects and their available footprint/evidence.
- **Pros:** Gives a clear starting point and connects a headline to inspected data.
- **Cons:** Static ≠ one-line/easy; JNI ≠ intrinsically hard; retained ≠ bytes safely freed. A numeric ROI score would imply unjustified precision.
- **Improve:** Rank by observed issue evidence, impact, confidence, and owner specificity; let experts pin/override priority. Merge with H2, show at most three distinct findings.
- **Size:** ΔJSON 0 B for client ranking; estimated ≤2 KiB gzip shared summary/links. Optional reusable evidence metadata belongs in H3's budget.
- **Rust:** 0 additional heap scan/scratch; existing-rule ranking only. Do not perform cut simulations for every suspect to manufacture ROI automatically.

### H2. Deterministic triage digest

- **Verdict:** Keep concise — likely useful to orient non-experts while preserving an evidence trail.
- **Analysis:** Basic existing triage/facts; optional-pass findings appear only with their actual data/coverage.
- **Pros:** A few sentences identify what occupies memory and where to inspect next without an LLM.
- **Cons:** “The leak is X” or unconditional cleanup instructions can turn a heuristic into a false diagnosis; a long digest duplicates tables and overwhelms.
- **Improve:** Three evidence-linked statements, “observed/suggests/unknown” wording, and no forced single culprit on diffuse heaps. Copy includes coverage/assumptions.
- **Size:** Client templates ΔJSON 0 B; estimated 1–4 KiB gzip template/link UI. If serialized for other clients, cap the extra text around 1–2 KiB.
- **Rust:** 0 new heap pass; optional deterministic text assembly O(fired rules + short rows). Existing triage evaluation is not a reason for another heap walk.

### H3. Coverage, limits, and provenance

- **Verdict:** Keep — essential correctness support, not a wall of alarming warnings.
- **Analysis:** Basic flags, input redaction/truncation, output/graph caps, unreachable-skip state, and existing record census. A consistent per-producer coverage registry is new metadata work.
- **Pros:** Distinguishes “not run,” “sampled,” “unsupported,” and a genuine checked negative result.
- **Cons:** Empty vectors/defaulted old flags are ambiguous; requested Full can be a stale cached report. Too many badges can obscure the actual finding.
- **Improve:** One compact summary/expandable matrix with actual enabled passes, producer completeness, and known limitations. Do not add obsolete thread/alloc boolean flags; record detail caps and genuine missing field-stats metadata.
- **Size:** Existing UI ΔJSON 0 B; proposed 20–30 producer states at ~150–250 B/state ≈3–8 KiB raw, excluding long diagnostics. Estimated 2–6 KiB gzip summary/matrix UI.
- **Rust:** O(number of producers) metadata state, typically KiB, not O(N) new scans. Producers must forward their real counters/truncation reasons; presence/count alone is not completion evidence.

### I3. Unreachable object composition

- **Verdict:** Contextual — helpful when recorded garbage is large, secondary to explaining the live set.
- **Analysis:** Basic unreachable shallow/composition and capped histogram; forest dominance can be skipped above 500K unreachable objects. CLI Full forces it, unlike current MCP/browser Full.
- **Pros:** Separates unreachable recorded storage from live owners and explains incomplete garbage-tree detail.
- **Cons:** Unreachable fraction is not physical fragmentation, allocation rate, GC throughput, or proof a GC is failing. A snapshot can intentionally include uncollected objects.
- **Improve:** Show recorded reachable/unreachable totals and the skipped/available tree state in Evidence. No automatic “force a full GC” prescription from the fraction alone.
- **Size:** ΔJSON 0 B for current data; estimated 1–4 KiB gzip shared composition/table/tree controls.
- **Rust:** View 0. Forcing unreachable dominance adds O(U+E_unreachable) work/state and can make a pathological dump much slower/larger in peak memory; don't enable it merely to draw a small chart.

### M2. JVM environment and decoded properties

- **Verdict:** Narrow/contextual — useful capture context under Evidence, not a complete VM-tuning panel.
- **Analysis:** Basic decoded properties/version/ID-width and any available qualified compression inference; missing properties stay unknown. Actual flags/capacity/collector inputs are separately deferred.
- **Pros:** Helps explain layout/version differences and reproducibility of an investigation.
- **Cons:** System properties are not VM arguments; ID width is not heap reference width. The property table can fail decoding and can contain sensitive values.
- **Improve:** Small masked/searchable property list, source/unknown labels, and no inferred `-Xmx`, collector, region size, or universal compressed-oop cliff.
- **Size:** ΔJSON 0 B; estimated 1–4 KiB gzip shared table/coverage UI. Do not duplicate environment strings in every chart.
- **Rust:** 0 new view work; actual external context ingestion needs a separately validated protocol and is not a cheap flag inferred from HPROF.

### N3. Heap-health KPI dashboard

- **Verdict:** Replace with neutral facts — useful orientation, unjustified as a green/red “health” score.
- **Analysis:** Basic recorded shallow totals, reachable/unreachable share, object count, and disjoint concentration milestones.
- **Pros:** Gives scale and distribution before users drill into owners.
- **Cons:** The historical `heap_fragmentation_ratio` is unreachable-byte proportion, not fragmentation; `total_shallow/total_objects` is average shallow size, not allocation density/time. Arbitrary loader/count thresholds create noise.
- **Improve:** At most three fact groups with explicit denominators, link to Consumers/Owners/Evidence, and no percent-of-`-Xmx` when capacity is unknown. Merge C4/C5/H2.
- **Size:** ΔJSON 0 B; estimated ≤2 KiB gzip shared fact row.
- **Rust:** 0 new pass/scratch; all arithmetic is over existing scalars. Runtime/GC “health” needs temporal external evidence and remains deferred.

## Shared exploration UX

These are the highest-leverage improvements because every retained view uses them.
They do not justify ~2,100 LOC as a precise forecast; size/effort depends on reuse.

### X1. Consistent hover and keyboard summaries

- **Verdict:** Keep — likely helps users understand a metric without changing screens.
- **Analysis:** Basic available facts; additional actions reflect actual capabilities.
- **Pros:** Exact bytes, denominator, scope, and provenance reduce mental arithmetic.
- **Cons:** A giant tooltip or hover-only action is inaccessible and hides detail.
- **Improve:** Short hover/focus summary plus visible Inspect button; no interactive actions inside pointer-events-none tooltips.
- **Size:** ΔJSON 0 B; estimated 2–6 KiB gzip shared tooltip/focus styles.
- **Rust:** 0 new pass/state; never fetch all paths on every hover.

### X2. Persistent selected-object inspector

- **Verdict:** Keep — more valuable than another chart.
- **Analysis:** Basic facts/available chain; explicit optional query actions for more detail.
- **Pros:** Users can copy class names/queries, compare metrics, and continue their investigation.
- **Cons:** A modal that traps every click or blocks the table becomes cumbersome.
- **Improve:** One non-blocking drawer with focus management, Escape/close, pinned selection, clear object/class scope, and unavailable-action explanations.
- **Size:** ΔJSON 0 B; estimated 4–10 KiB gzip shared inspector/navigation, reused across all views.
- **Rust:** 0 new pass/state until an explicit indexed query is requested; query budgets/coverage are visible.

### X3. Context menu

- **Verdict:** Secondary/merge — convenient for experts, unnecessary as the only way to discover core actions.
- **Analysis:** Same capabilities as the visible inspector actions.
- **Pros:** Reduces pointer travel for repeated inspection/copy operations.
- **Cons:** Hidden on mobile/keyboard-only workflows; nine actions everywhere overload.
- **Improve:** Visible Inspect/Why retained/Copy first; optional small keyboard-accessible menu with only available actions.
- **Size:** ΔJSON 0 B; estimated 1–3 KiB gzip incremental menu behavior.
- **Rust:** 0; do not trigger queries simply to decide every menu label.

### X4. Persistent cross-links and scope

- **Verdict:** Keep — likely essential for translating a class-sized footprint into ownership evidence.
- **Analysis:** Basic matching IDs/names plus optional inspectors; loader-distinct identity stays visible.
- **Pros:** Maintains context from summary to class, object, container, and thread evidence.
- **Cons:** Global auto-filtering or flashing every section changes denominators and can lose the user's place.
- **Improve:** Explicit shared selection, scoped filters, breadcrumbs/back/reset, and a deep link; distinguish class name from object identity.
- **Size:** ΔJSON 0 B; estimated 2–6 KiB gzip shared state/navigation.
- **Rust:** 0 new pass/state; existing data is linked, not re-exported once per destination.

### X5. Ownership/root-path explorer

- **Verdict:** Keep — central answer to why an object is retained.
- **Analysis:** Basic available dominance chain; actual edge-labeled/all-root paths need a producer/query that returns that evidence. Full exported graph is still capped.
- **Pros:** Moves from retained size to lifetime owner and a concrete inspection target.
- **Cons:** Dominance arrows cannot be labeled as fields unless verified; one path does not establish sole ownership, and exhaustive all-root enumeration can explode.
- **Improve:** Separate ownership/reference modes, scope labels, progressive bounded expansion, alternative-referrer evidence, and copyable chain. Never enumerate everything by default.
- **Size:** Existing ΔJSON 0 B; estimated 2–8 KiB gzip shared path rendering. New requested path results use ≤32 KiB raw output/work limits and truncation states.
- **Rust:** Existing view 0. New graph/path queries can be O(N+E) or output-explosive, with visited/frontier state; enforce depth/path/work budgets and unknown/incomplete results.

### X6. One-click “Why retained?”

- **Verdict:** Keep/merge with X2/X5 — primary next action, not a separate visualization implementation.
- **Analysis:** Uses the available object/representative-class evidence; compute missing paths only on explicit request.
- **Pros:** Shortens the most common investigation step.
- **Cons:** Class-wide retention cannot be explained by one representative object's chain without a scope label.
- **Improve:** “Inspect top instance's ownership” when only a sample exists; no pretending a representative is the entire class or sole retaining route.
- **Size:** ΔJSON 0 B; estimated <1–2 KiB gzip action/deep-link wiring, sharing the inspector/path component.
- **Rust:** 0 extra work for available chains; missing path work is X5's explicit query, not a global pass per class.

### X7. Per-idea interaction policy

- **Verdict:** Keep only minimal shared UX guidance, not a feature/panel or a mandatory action set in every view.
- **Analysis:** Applies to all existing/new views; does not enable data.
- **Pros:** Stops each chart inventing different click/copy/denominator rules.
- **Cons:** Exhaustive hover/context menus in every chart multiply UI complexity.
- **Improve:** Share essential metric/scope labels and relevant inspect/link/copy behavior; let each view choose its own controls. No universal hover or context-menu checklist.
- **Size:** 0 B report data; 0 additional bundle beyond X1–X6.
- **Rust:** 0 passes/scratch/runtime.

## Triage: improve evidence before adding warnings

**Proposals below are not implemented Rust rules in this change.** Existing
[rule registry](src/report/triage.rs#L332-L406) already covers concentration,
wrappers, empty/sparse collections, duplicate strings/arrays, ThreadLocals,
large arrays, exceptions, and many framework patterns. Adding another badge
for each chart would repeat evidence, not improve diagnosis.

### Correct current wording/metrics first

| Existing signal / claim | Problem | Proposed correction |
| --- | --- | --- |
| `hash-collision-hotspot` | Current producer uses nonempty buckets/table length, but rule calls it load factor/long collision chains | Rename to neutral bucket-occupancy evidence; only assert extreme chains after a verified per-map walk |
| `humongous-object-allocation` | Fixed large-array threshold does not verify G1 collector/region size | “Large primitive array,” actual bytes/owner; defer G1 claims |
| `finalizer-queue-backlog` | Finalizer population ≠ pending queue or stalled thread | “Finalizer population”; pending/backlog only with verified queue evidence |
| `gc-waste` / fragmentation wording | Unreachable recorded fraction ≠ throughput, physical fragmentation, or mandatory GC action | Recorded unreachable storage and capture/skip caveats; no automatic `GC.run` prescription |
| `thread-local-spread-leak` | Thread-object retention plus local roots does not prove all bytes are ThreadLocal values | Worker-thread retention evidence; link actual stale-entry detail before prescribing cleanup |
| `exception-object-accumulation` | Exception/Error name suffix is not subtype membership or trace-storage cost | Verify Throwable subtype through supported metadata/query, preserve a name-heuristic fallback label |
| Classloader/metaspace/native-fragmentation claims | Definition/buffer counts do not establish expected loader death, native usage, or physical fragmentation | Population/ownership facts only; lifecycle/native-runtime claims stay out of dump-only scope |
| String slack / duplicate “savings” | UTF-8 decoded length is not UTF-16/Latin1 physical used bytes; shared storage/overlap is unresolved | Fix encoding-aware accounting and verification before exact opportunity claims |

[Occupancy producer](src/pass2/fielddecode.rs#L1694-L1710),
[collision rule](src/report/triage.rs#L2355-L2392),
[String storage calculation](src/pass2/strings.rs#L43-L74),
[thread retention source](src/report/build.rs#L2418-L2433).

### Candidate evidence signals / refinements

Only the retained proposals below remain candidates; the three dropped signals
and deferred String analysis are listed in the [decision record](#triage-decisions).
Thresholds are **proposed, tunable policy**, not facts or implemented constants.
Require both meaningful absolute bytes and scoped share where possible; show
counterexamples and suppress a generic rule when an owner-specific refinement
already explains the same evidence.

All candidate signals below are constrained to fields computable from the heap
dump and existing report model/query outputs.

| Proposal | Gate / evidence and conservative trigger | False-positive / suppression | Incremental size / Rust cost |
| --- | --- | --- | --- |
| Coverage-limited result | H3 producer state: meaningful input truncation, capped eligible scan, stale profile or required decoding absent | An output top-N cap alone is not scan failure; one grouped informational notice, not a critical badge per section | Shared 3–8 KiB raw metadata; O(producers), no heap scan |
| Stale ThreadLocal value footprint | Refine existing E2: unique stale-value shallow/qualified retained evidence, e.g. ≥8 MiB and ≥1% recorded reachable bytes, with repeated stale entries | Cleanup may be delayed until next access; do not attribute all value-class row bytes to stale entries. Suppress generic null-key-only headline when refined evidence exists | ≤8 KiB raw; O(captured entries), bounded value dedup state; Duplicates/new refinement, not Basic scalar alone |
| Repeated empty-field overhead | Refine D4/TinyCollectionOverhead: a named field contributes e.g. ≥8 MiB and ≥1% estimated structural overhead, with a substantial repeated population | Intentional mutable/lazy/framework containers; do not promise a safe null/immutable replacement. Collapse generic empty-container notices | Existing rows 0 B; optional provenance ≤4 KiB; no new scan for already captured attribution |
| Large array with actionable owner | Refine OversizedPrimArray/N1: measured size e.g. ≥32 MiB or ≥5% of recorded reachable bytes, plus actual owner/representative path where available | Deliberate bounded large payload; merge duplicate generic large-array/humongous notices | Existing samples 0 B; no new default pass; missing full-population results stay unknown |
| Application superclass-family footprint | F3 new metadata: user-declared application family, ≥5 concrete descendants and e.g. ≥64 MiB / ≥10% reachable shallow bytes | Generic framework/Object bases and legitimate domains; contextual query/fact, not a red leak score. Suppress duplicated child headlines | Thin O(C) links/counters; O(C) postorder; no additional per-object array if fused |
| Finalizable population by class | P2 replaces count-only `finalizer-queue-backlog`: ≥10,000 Finalizer objects **and** one referent class ≥40% of them | Live finalizable resources are normal; never claim backlog or a stalled thread without a decoded queue | ≤12 KiB raw; O(Finalizer instances) in the existing reference decode |

**Do not add** automatic signals for cycles alone, WAITING/BLOCKED state alone,
unknown cache policy, zero-instance classes, a high retained/shallow multiplier,
address gaps, hypothetical G1 regions, or time-to-OOM. The user explicitly
dropped the shared-retainer caution, business-bucket skew, and queued-work
retention triage signals; their underlying B2/K5/P1 investigative views remain.
A discovered negative result must describe what was actually checked.

## Implementation order and acceptance gates

1. **Correctness and scope:** H3/metric labels/cached-profile checks, X2/X4/X5/X6,
   no additive retained/waste totals, no obsolete Basic/Full claims.
2. **Cheap useful slices:** existing Consumers/Owners tables, A2/A3 selection,
   L3/D4 detail, E2/E3/E4, shared opportunity list, exact C5 milestones.
3. **Contextual modes:** small A1, C3/K3/D2, supported K5 queries, comparison G1.
4. **New producers after layout verification:** P2 finalizer classes and P4
  verified buffer backing, then descriptive P1 for supported
  `ThreadPoolExecutor` and HikariCP layouts; no queued-work triage signal.
5. **New heap-only producers only after sizing:** F3 links, selected true
  map-bin/ownership queries, P3, verified queue/trace detail. Encoding-correct
  D3 String accounting remains a separate deferred investigation.
6. **Separate ideas file stays separate** until dump-derived evidence, bounded
   schema, and measured ThinkStation costs justify re-entry.

### Acceptance checklist

- All 62 remaining original IDs (G4, N5, N6 removed) plus P1–P4 have exactly one current assessment, either in
  this review or in the separate-ideas file. The appendix is history, not an assessment.
- Every kept/deferred ID has a user verdict and a “shown when” condition; views
  without data are hidden, not shown empty. Dropped ideas record why and are never shown.
- Kept/deferred assessments state verdict, analysis gate, pros, cons,
  improvement, and **estimated** size/Rust runtime/scratch implications.
  Dropped ideas need only a decision and reason; shared UX is assessed too.
- Existing tables/implemented data are acknowledged; no “0% visualized” marketing claims.
- Basic and each entry point's Full flags come from source; newer producer work
  is never advertised as already enabled by Full.
- Core report has clear task-first navigation without a numeric destination
  cap, reuses one inspector, and has a short evidence digest; contextual charts
  do not render all at once.
- Prototype math reconciles additive shallow totals and disjoint owners, preserves
  unshown tree mass, uses common sparkline scales, and never sums overlapping opportunities.
- Keyboard focus/Escape, responsive layouts, reduced motion, no CDN requirement,
  and old demo-link routing are exercised in browser tests.
- Fresh Rust memory/runtime benchmarks and new production triage tests remain
  future implementation work on ThinkStation; documentation estimates are not measurements.

## Source grounding

- [src/opts.rs](src/opts.rs): executable option defaults, detail/size caps.
- [src/main.rs](src/main.rs): CLI folding, size override, field-stat CSR copy, pipeline frees.
- [src/mcp/mod.rs](src/mcp/mod.rs) and [crates/hprof-wasm/src/lib.rs](crates/hprof-wasm/src/lib.rs): nonidentical Full capabilities.
- [src/report/model.rs](src/report/model.rs), [src/report/build.rs](src/report/build.rs): actual fields, limits, nested retained semantics, ordered release.
- [src/pass2/fielddecode.rs](src/pass2/fielddecode.rs), [src/pass2/strings.rs](src/pass2/strings.rs): summaries, occupancy proxy, payload decoding and caps.
- [web/src/App.tsx](web/src/App.tsx): existing tables/sections, not an absence claim based on the old catalog.
- [src/diff_reports.rs](src/diff_reports.rs), [docs/OQL.md](docs/OQL.md): name-based series and existing query/grouping capabilities.
- [src/pass2/framework_scan.rs](src/pass2/framework_scan.rs), [src/report/triage.rs](src/report/triage.rs): class-name pool sentinels and current finalizer/off-heap rules (P1–P4 baseline).
- [VISUALIZATION_IDEAS_DEFERRED.md](VISUALIZATION_IDEAS_DEFERRED.md): separate ideas and re-entry criteria.
- [VISUALIZATION_IDEAS_SUMMARY.md](VISUALIZATION_IDEAS_SUMMARY.md): one-page index of all ideas.

---

## Appendix: original idea catalog (superseded)

> **Superseded and partial.** This is the pre-review catalog, kept for
> traceability of the original wording. Only its baseline, library audit,
> exploration framework (X1–X7), and category A entries survive; categories
> B–N, the D-contracts, roadmap, and sources were not preserved. Every ID has a
> current assessment in the review above or in
> [VISUALIZATION_IDEAS_DEFERRED.md](VISUALIZATION_IDEAS_DEFERRED.md). Where this
> appendix conflicts with the review, the review wins.

Comprehensive catalog of views, charts, and analyses to help users discover
*what* uses heap memory and *why*. Each idea is judged on value, feasibility,
and fit with the existing codebase. The catalog contains **65 entries across
14 categories**, plus six shared exploration-infrastructure components.

**Key principle: prefer existing libraries; allow targeted additions where they
bring dramatic quality improvement.** After deep research into JProfiler, YourKit,
MAT, VisualVM, HeapHero, LeakCanary, Perfetto, Chrome DevTools, and academic tools,
earlier research added categories J–N for waste, field-level retention, collection
quality, security, and GC tuning. The latest gap audit incorporates **all 17 further
proposals: 11 new entries and six extensions**, organized into the existing
categories rather than duplicating related views. See the
[integration map](#latest-gap-audit-integration) for each proposal's destination.
Two optional packages remain recommended: `@nivo/sunburst` (animated drill-down
dominator view, the most impactful new visualization) and `d3-flame-graph` (allocation
flame graphs, industry standard). None of the 17 new/extended proposals requires
a new frontend chart package; heavy analysis and capture integrations are separate
backend prerequisites, not hidden in chart-only estimates.

**Working demos:** [`docs/visualization-demos.html`](docs/visualization-demos.html)
contains interactive prototypes of the top 10 ideas, all built with existing
libraries (d3-hierarchy, d3-sankey, Chart.js, pure SVG/CSS).

---

## Table of Contents

- [Current State Baseline](#current-state-baseline)
  - [Latest Gap-Audit Integration](#latest-gap-audit-integration) — all 17 proposals mapped to existing categories
- [Library Audit](#library-audit) — what we have vs. what we need
- [Exploration Framework](#exploration-framework) — interaction standards for all views
  - [X1. Rich Tooltip Standard](#x1-rich-tooltip-standard-)
  - [X2. Pinnable Tooltip / Detail Popover](#x2-pinnable-tooltip--detail-popover-)
  - [X3. Context Menu](#x3-context-menu-)
  - [X4. Universal Class Crosslinks](#x4-universal-class-crosslinks-)
  - [X5. Dominator Chain Explorer](#x5-dominator-chain-explorer-)
  - [X6. "Why Is This Alive?" One-Click Flow](#x6-why-is-this-alive-one-click-flow-)
- [Idea Catalog](#idea-catalog) — 65 entries in 14 categories
  - [A — Dominator & Ownership Structure](#a--dominator--ownership-structure) (6 entries)
  - B — Flow & Path Views (4 entries)
  - C — Histogram & Distribution Views (5 entries)
  - D — Collection & Waste Views (6 entries)
  - E — Thread & Concurrency Views (5 entries)
  - F — ClassLoader & Framework Views (3 entries)
  - G — Diff & Temporal Views (4 entries)
  - H — Summary & Decision Views (3 entries)
  - I — Currently Unvisualized Data (4 entries)
  - J — Advanced Waste Detection (7 entries)
  - K — Field & Retention Intelligence (5 entries)
  - L — Collection Intelligence (4 entries)
  - M — Security & Metadata Views (3 entries)
  - N — GC Tuning Insights (6 entries)
- Suggested New JSON Data — D-1–D-27 backend/capture contracts
- Implementation Roadmap
- Verdict Summary Table
- Research Sources

---

## Current State Baseline

The baseline inventory records the earlier audit. Some data now has table or
partial rendering; recheck the current viewer before implementing a proposed
chart. A missing guided view does not necessarily mean the parser/query engine
lacks the underlying capability.

### Existing views

| View | Report section | Chart component | Data type |
|------|---------------|----------------|-----------|
| Class histogram | System Overview | `FlatTreemap` + table | `HistRow[]` |
| Heap composition | System Overview | `FlatTreemap` | `KindStat[]` |
| Loader rollup | System Overview | `FlatTreemap` | `LoaderRollup[]` |
| Retention concentration | System Overview | `VBar` | `RetentionSummary` |
| Dominator depth | System Overview | `VBar` | `DepthBucket[]` |
| GC roots by type | System Overview | `FlatTreemap` | `GcRootTypeRow[]` |
| Leak suspects | Leak Suspects | tree + DAG | `Suspect[]`, `MergedPathNode` |
| Dominated subtree | Leak Suspects | `d3-dag` sugiyama SVG | `DomTreeNode` |
| Top consumers | Top Consumers | `ZoomableTreemap` + flame | `ObjRow[]`, `ClassRow[]` |
| Retention chains | Top Consumers | multi-hop holder table | `HolderRow[]` |
| Package tree | Top Consumers | `RetainedTreemap` | `PackageNode` |
| Imm. dominator pairs | Dominator Analysis | table | `ImmDomPair[]` |
| Big drops | Dominator Analysis | table | `BigDropRow[]` |
| Reference graph | Object Graph | Cytoscape | `ObjGraphFlat` |
| Type-level graph | Object Graph | Cytoscape | `TypeEdge[]` |
| Collection fill ratios | Collections | `FlatTreemap` + bars | `FillRatioBucket[]` |
| Biggest collections | Collections | table | `BiggestCollectionRow[]` |
| Container attribution | Collections | table | `FieldAttributionRow[]` |
| Thread overview | Threads | table | `ThreadInfo[]` |
| Duplicate strings | Duplicates | table + histogram | `DupStrings` |
| Diff view | Compare | growth-leaders table | `SeriesDiffResult` |
| Triage signals | Triage | badge list | `TriageSignal[]` |

### Previously unvisualized data (earlier report audit)

These report data sources were identified as visualization targets in the earlier
audit. Some now have tables/partial views; richer chart and drill-down proposals
remain useful, but this list is not an up-to-date absence test:

| Data source | Type | Always-on? |
|------------|------|:----------:|
| `category_rollup` | `CategoryRollup[]` | Yes |
| `framework_analysis` | `FrameworkAnalysis[]` | Yes (when detected) |
| `waste_summary` | `WasteSummary` | Yes (when > 0) |
| `self_referential_classes` | `SelfReferentialClassRow[]` | Yes |
| `references` (soft/weak/phantom) | `ReferencesAnalysis` | Yes |
| `thread_local_analysis` | `ThreadLocalLeakRow[]` | Opt-in |
| `size_distribution` | `TopSizeDistribution` | Yes |
| `arrays_by_size` | `ArraysBySize` | Yes |
| `unreachable_histogram` | `UnreachableHistRow[]` | Yes |
| `unreachable_composition` | `KindStat[]` | Yes |
| `alloc_sites` | `AllocSites` | Rare |
| `duplicate_classes` | `DuplicateClass[]` | Yes |
| `nio_buffer_owners` | `NioBufferOwner[]` | Yes |
| `field_stats` | `FieldStats` | Opt-in |

### Latest Gap-Audit Integration

All 17 follow-up proposals are preserved below. Six extend related entries;
eleven warrant separate entries. Existing OQL grouping, String decoding,
zero-instance histogram rows, and monitor-root parsing are reused rather than
incorrectly classified as missing backend capabilities.

| Audit | Capability | Catalog location | Treatment / prerequisite |
| --- | --- | --- | --- |
| O1 | Multi-dump growth regression / advisory time-to-OOM | G1 | Extend sparklines; timestamps/comparability D-13, verified budget D-23. |
| O3 | Shared ownership / single-reference cut impact | B2 | Extend blame view; complete-graph ownership/counterfactual D-14. |
| O4 | Compact-string / Latin1-compatible UTF-16 audit | D3 | Extend String classifier; encoding/unique backing storage D-15. |
| O5 | Duplicate immutable/value objects | J6 | New opt-in allowlisted equality analysis D-16. |
| O6 | Group-by-field value memory attribution | K5 | New guided view over existing OQL; union/coverage contract D-17. |
| O8 | Retention Pareto / Lorenz curve | C5 | New exact-milestone view; bounded disjoint-population curve D-26. |
| O9 | Class-hierarchy / superclass rollup | F3 | New hierarchy view; preserve loader-distinct definitions D-19. |
| O10 | Object-level reference-cycle / SCC detection | [A6](#a6-object-level-reference-cycles--scc-explorer-) | New opt-in graph analysis D-20; cycles alone are not leaks. |
| O11 | Loaded classes with no direct live instances | F1 | Extend loader view; existing histogram filter, richer metadata D-19. |
| O12 | Monitor-held/rooted object retention | E5 | New root-object view D-21; owners/waiters require external evidence. |
| O13 | Cache inventory / eviction policy audit | C2 | Extend framework footprint; versioned policy adapters D-22. |
| O15 | Throwable / stack-trace retention cost | J7 | New query-backed census; detailed trace/owner aggregation D-24. |
| O16 | Thread-stack depth / repeating recursion | E4 | Extend/move the former locals addendum; existing frames suffice for observed depth. |
| O17 | Global coverage, limits, and confidence | H3 | New unified status view over existing evidence plus registry D-25. |

**Reading rule:** "Data ready" below can mean a basic census/chart is available,
not that its richer analysis is implemented. Each extension separates those
stages and states its limits. Heavy or instrumented features remain conditional;
the catalog is a set of proposals, not a promise that all 65 views ship today.

---

## Library Audit

### Decision: existing stack; two optional targeted additions

After auditing every idea against the installed packages AND researching
competing tools (JProfiler, YourKit, MAT, HeapHero, Perfetto, Chrome DevTools),
**most of the 65 catalog entries use existing chart components/libraries**.
All 17 latest additions/extensions do so. Two optional additions remain
recommended for the sunburst/allocation-flame UX; their fallback implementations
also use the existing stack. No chart dependency solves the separate full-graph,
payload-decoding, stable-identity, or external-telemetry requirements:

### Installed packages (`web/package.json`)

| Package | What it provides | Used for |
|---------|-----------------|----------|
| `d3-hierarchy` | `partition()`, `hierarchy()`, `treemap()` | Sunburst (partition + manual SVG arcs) |
| `d3-sankey` | Sankey layout (nodes, links, positions) | Retention Flow Sankey (layout + SVG) |
| `d3-force` | Force-directed simulation | Currently unused; available for graph layouts |
| `d3-dag` | Sugiyama DAG layout | `domTree.tsx` |
| `cytoscape` | Graph visualization engine | Object graph, type-level graph |
| `cytoscape-cose-bilkent` | Layout algorithm for Cytoscape | Graph layouts |
| `chart.js` + `react-chartjs-2` | Pie, Bar, Scatter, Line | All standard charts |
| `react-data-table-component` | Sortable tables | Sortable data tables |

### Recommended new packages (targeted additions)

| Package | gz size | Why | What it replaces |
|---------|:-------:|-----|-----------------|
| `@nivo/core` + `@nivo/sunburst` | ~190 KB | Animated drill-down sunburst with breadcrumbs, tooltips, and arc transitions. JProfiler added sunburst in v11.1 (2022) and it's now their most-used heap view. Manual `d3-hierarchy` arcs work (see demo) but lack animation, drill-down breadcrumbs, and responsive sizing — 300 LOC manual vs 60 LOC with Nivo. **Quadratic bug (GitHub #2863) is mitigated by depth-limiting dominator trees to 6 rings, which we do anyway.** | 300 LOC manual SVG arc code |
| `d3-flame-graph` | ~35 KB | Industry-standard flame graph: click-to-zoom, search/highlight, tooltips, inverted icicle mode. Used by Netflix, Jaeger, Google pprof. The existing `ZoomableTreemap` flame mode can be adapted (~80 LOC) but lacks the search API and proper flame semantics. For allocation site visualization (#I4), the dedicated library is worth 35 KB. **TypeScript gotcha:** no `@types` — write a 20-line local `.d.ts` file (Jaeger's pattern). | Adapting ZoomableTreemap (~80 LOC but worse UX) |

```sh
# Install (optional — only needed for A1 Sunburst + I4 Flame Graph):
cd web && npm install --ignore-scripts @nivo/core @nivo/sunburst d3-flame-graph
# Total addition: ~225 KB gzipped (vs ~635 KB for full Nivo suite in draft 1)
```

### Chart type → library mapping (mostly existing packages)

| Chart type | Library | How | Bundle cost |
|-----------|---------|-----|:-----------:|
| **Sunburst** | `@nivo/sunburst` (recommended) or `d3-hierarchy` (fallback) | Nivo: `<Sunburst data={tree} />` with animated drill-down. Fallback: `partition().size([2π, radius])` + manual SVG arcs. | ~190 KB (Nivo) or 0 KB (manual) |
| **Sankey** | `d3-sankey` (installed) | Layout-only → render SVG paths + rects manually. ~80 LOC. | 0 KB |
| **Heatmap** | Pure CSS | HTML `<table>` + `rgba()` cell backgrounds. ~30 LOC. No library needed at all. | 0 KB |
| **Chord** | ~~`@nivo/chord`~~ | **Dropped.** Heatmap (#A2) serves the same purpose more readably. | — |
| **Scatter** | `chart.js` (installed) | Register `ScatterController` + `PointElement` in `chartSetup.ts` (+2 imports, +2 in register call). Already in the chart.js bundle. | 0 KB |
| **Waterfall** | Pure SVG | Manual `<rect>` elements. ~50 LOC. | 0 KB |
| **Sparklines** | Pure SVG `<polyline>` | 10–15 lines of code per sparkline. | 0 KB |
| **Flame graph** | `d3-flame-graph` (recommended) or adapt `ZoomableTreemap` (fallback) | d3-flame-graph: industry standard with search API. Fallback: adapt existing flame/icicle toggle. | ~35 KB (d3-flame-graph) or 0 KB (adapt) |
| **Treemaps** | `FlatTreemap` / `ZoomableTreemap` | Already excellent; no replacement needed. | 0 KB |
| **Bars (horizontal/vertical)** | `chart.js` (installed) | `BarElement` already registered. `VBar`, `HBar` components exist. | 0 KB |
| **Stacked bars** | `chart.js` (installed) | `StackedBar` component already exists in `charts.tsx`. | 0 KB |
| **Node graph** | `cytoscape` (installed) | Already used for reference graph. | 0 KB |

### What was dropped from the Nivo recommendation

| Previous recommendation | Replacement | Savings |
|------------------------|-------------|:-------:|
| `@nivo/sunburst` (~190 KB) | `d3-hierarchy partition()` + 40 LOC SVG | 190 KB |
| `@nivo/sankey` (~55 KB) | `d3-sankey` (installed) + 80 LOC SVG | 55 KB |
| `@nivo/heatmap` (~48 KB) | Pure CSS `<table>` + `rgba()` | 48 KB |
| `@nivo/chord` (~55 KB) | Dropped entirely (heatmap is better) | 55 KB |
| `@nivo/scatterplot` (~50 KB) | Chart.js `ScatterController` (already bundled) | 50 KB |
| `@nivo/core` (~140 KB) | Not needed | 140 KB |
| `@xyflow/react` (~57 KB) | Cytoscape (installed) or `d3-hierarchy tree()` | 57 KB |
| `d3-flame-graph` (~40 KB) | Adapt `ZoomableTreemap` flame mode | 40 KB |
| **Total saved** | | **~635 KB gz** |

### Scatter plot: one-time setup in `chartSetup.ts`

To enable Chart.js scatter charts, add two imports and two register calls:

```typescript
// In chartSetup.ts — add to the existing import:
import {
  // ... existing imports ...
  ScatterController,
  PointElement,
  LineElement,      // for sparkline/line charts
  LineController,   // for sparkline/line charts
} from "chart.js";

ChartJS.register(
  // ... existing registrations ...
  ScatterController, PointElement, LineElement, LineController,
);
```

This is a one-time change. `ScatterController` and `PointElement` are already in
the `chart.js` bundle — registering them adds zero extra bytes to the final bundle.

---


---

## Exploration Framework

*Every view in this tool should answer the user's next question without forcing
them to switch tabs, copy-paste a class name, or mentally correlate two distant
tables. This section defines the interaction standards that all ideas — existing
and proposed — must follow.*

**Design philosophy:** This is an expert tool. Users are JVM engineers hunting
production memory leaks. They want to start from a high-level signal (triage,
histogram, treemap), drill into the structural cause (dominator chain, retention
path, field edges), and reach a specific object or code location. Every click
should take them deeper. Every tooltip should show the one thing they'd otherwise
have to compute in their head.

### X1. Rich Tooltip Standard 🟢

**Current state:** Inconsistent. `ZoomableTreemap` uses the `<Tooltip>` component
with `TipHeader`/`TipRow`/`TipNote`. But `FlatTreemap`, `LeakShareChart`,
`RootPathChain`, and `DomSubtreeSvg` all use native browser `title` attributes —
which render with a 300 ms delay, disappear on mouseout, have no formatting, and
cannot contain links or action buttons.

**Target state:** Every hoverable element uses the `<Tooltip>` component with a
standard rich layout:

```
┌─────────────────────────────────────────┐
│ java.util.HashMap$Node[]           ⎘ ⬡  │  ← TipHeader + inline actions
├─────────────────────────────────────────┤
│ Retained    124.8 MB  (38,192 exact)    │  ← TipRow (primary metric)
│ Shallow       2.1 MB  (exact)           │  ← TipRow (secondary)
│ Instances     12,847                    │  ← TipRow (count)
│ % of heap     34.2%                     │  ← TipRow (context)
│ Field         .table                    │  ← TipRow (edge label)
│ Held by       com.app.UserCache         │  ← TipRow (dominator)
├─────────────────────────────────────────┤
│ Click to drill in ▶                     │  ← TipNote (action hint)
│ Right-click for more actions            │  ← TipNote (context menu hint)
└─────────────────────────────────────────┘
```

**Rules:**

| Element type | Required tooltip fields |
|-------------|----------------------|
| Class name (in any table) | Retained (exact), Shallow (exact), Instances, % of heap, Category |
| Treemap tile | Retained (exact), % of parent, % of total, Children count, Dominator class |
| Sankey node | Retained flow (exact), Inbound edges, Outbound edges, Top holder |
| Heatmap cell | Pair count, Dominated retained (exact), % of total, Dominator class + Dominated class |
| Bar chart segment | Value (exact), % of total, Label, Rank |
| Scatter plot point | Both axis values (exact), Class name, Instances, Outlier flag |
| GC root path step | Class name, Retained (exact), % of suspect, Field edge, Step N of M, GC root type (last step) |
| Dominator tree node | Class name, Retained (exact), Shallow (exact), Children count, Depth, Parent class |
| Thread bar | Thread name, State, Retained (exact), % of heap, Dominant class, Local root count |

**Migration effort:** ~200 LOC. Replace all native `title` attrs with `<Tooltip>` in:
`FlatTreemap` (~40 LOC), `LeakShareChart` (~20 LOC), `RootPathChain` (~40 LOC),
`DomSubtreeSvg` (~30 LOC), `StackedBar` (~20 LOC), `RetentionChainBar` (already done),
plus add dominator-class and field-edge fields to `ZoomableTreemap` tooltips (~50 LOC).

---

### X2. Pinnable Tooltip / Detail Popover 🟢

**Current state:** All tooltips have `pointerEvents: "none"` — they vanish the
instant the cursor leaves the trigger element. Users cannot copy text, click
links, or interact with tooltip content in any way.

**Target state:** Two-tier system:

1. **Hover tooltip** (existing behavior) — appears on hover, disappears on leave.
   Still `pointerEvents: "none"`. Shows summary metrics only.

2. **Detail popover** — appears on **click** (not hover). Stays open until
   dismissed (click elsewhere or press Esc). Has `pointerEvents: "auto"`.
   Contains:
   - All tooltip fields plus additional detail
   - Copy buttons for class name, OQL, object ID
   - Action buttons: "Show in Histogram", "Explore in Object Graph", "Show GC Root Path"
   - Mini retention chain (if `root_path` available for this class)
   - Expandable dominated-objects list (if dominator tree available)

```
┌─────────────────────────────────────────────┐
│ java.util.HashMap$Node[]              ✕     │
├─────────────────────────────────────────────┤
│ Retained    124.8 MB  (130,809,856 bytes)   │
│ Shallow       2.1 MB  (2,203,648 bytes)     │
│ Instances     12,847                        │
│ % of heap     34.2%                         │
│ Category      jdk                           │
│ Loader        <boot>                        │
├─────────────────────────────────────────────┤
│ GC Root Chain (top instance):               │
│  ① UserCache          ← .table             │
│  ② AppServlet         ← .cache             │
│  ③ Thread "http-8080" ← Java Frame         │
├─────────────────────────────────────────────┤
│ [⬡ Inspect] [⬡≡ Instances] [⌗ OQL] [⎘]    │
│ [Show in Histogram →] [Show Holders →]      │
└─────────────────────────────────────────────┘
```

**Implementation:** Extend `tooltip.tsx` with a `<Popover>` variant. On the
trigger element: hover → tooltip, click → popover (with `useFloating` +
`useClick` from `@floating-ui/react`). The popover reads `root_path` from the
histogram top-20 rows (already in the report JSON for the biggest classes).

**Effort:** ~250 LOC for the Popover component + ~150 LOC to wire it into the
top-20 histogram rows, top consumers, and suspect cards. Total ~400 LOC.

---

### X3. Context Menu 🟢

**Current state:** `StdTable` has `onRowContextMenu` wired up (line 250–260 in
`charts.tsx`) but **no caller ever passes this prop**. No context menu UI exists
anywhere in the codebase.

**Target state:** Right-click any class name, table row, treemap tile, or chart
element → context menu with:

| Action | Icon | Behavior | Available when |
|--------|------|----------|---------------|
| Show in Histogram | 📊 | `pivotClass(cls)` + `navTo("system-overview")` + highlight | Always |
| Show Holders | ⛓ | `navTo("dominator-analysis")` + pivot to ImmDom pairs for this class | Always |
| Show GC Root Path | 🌳 | Open popover with `root_path` for top instance | When `root_path` available (top-20 classes) |
| Find via Field | 🔍 | `fireInspect({ kind: "field-scan", cls, fieldName })` | When WASM available |
| Explore Instance | ⬡↗ | `fireInspect({ kind: "instance", idx, cls })` | When ObjGraph available |
| Show in Dominator Tree | ⌞ | `navTo("domtree/" + denseIdx)` | When ObjGraph available |
| Instances in Inspector | ⬡≡ | `fireInspect({ kind: "instances", cls, page: 0 })` | When WASM available |
| Copy Class Name | ⎘ | `navigator.clipboard.writeText(cls)` | Always |
| Copy OQL | ⌗ | Copy `SELECT * FROM cls LIMIT 20` | Always |

**Implementation:** A shared `<ContextMenu>` component using `@floating-ui/react`
(already installed for tooltips). Each table/chart passes an `onContextMenu`
callback that provides the class name, object index, and available fields.

**Effort:** ~200 LOC for the ContextMenu component + ~100 LOC per section to
wire it in. Total ~600 LOC for full coverage.

---

### X4. Universal Class Crosslinks 🟢

**Current state:** `pivotClass()` fires `"pivot-class"` CustomEvent + navigates
to `#dominator-analysis` + highlights in histogram. But this is one-directional
and only hits two sections.

**Target state:** Clicking a class name (or choosing "Show in…" from context
menu) highlights that class in ALL relevant sections simultaneously:

| Target section | How class is highlighted |
|---------------|------------------------|
| Class Histogram | Yellow flash (3s), scroll into view (already works via `highlight-class`) |
| Top Consumers: Biggest Classes | Yellow flash (3s), scroll into view |
| Top Consumers: Package Treemap | Zoom to package, highlight class tile |
| Dominator Analysis: ImmDom Pairs | Filter/highlight rows where class is dominator or dominated |
| Dominator Analysis: Big Drops | Highlight rows where class is the drop object |
| Collections: Biggest | Filter rows where `container_class` matches |
| Collections: Container Attribution | Filter rows where `holder_class` matches |
| References: Soft/Weak/Phantom | Highlight rows where `pretty_class` matches |
| Thread Overview | Highlight threads where `dominant_retained_class` matches |

**Implementation:** Extend `highlight-class` CustomEvent to be received by all
sections (not just ClassHistogramTable). Each section registers a listener that
applies a temporary highlight style. ~50 LOC per section × 8 sections = ~400 LOC.

---

### X5. Dominator Chain Explorer 🟢

**Current state:** `RootPathChain` renders a single GC root path (SVG boxes +
arrows) for suspects and top-20 histogram classes. Clicking a box navigates to
the Object Graph Explorer if available. Field edges are shown as text labels on
arrows. The last box shows `root_type_label`. But:

- **Only one path** is shown (the shortest). The WASM explorer can show all paths,
  but only inside its own panel — not inline in suspect cards or histogram rows.
- **Field edges are not clickable** — you can't "find all objects held via `.table`"
- **Root type label is not clickable** — "Java Frame" doesn't link to the thread
- **DomSubtreeSvg prunes at depth 1** when >25 nodes, with no expand button
- **No way to copy** the chain as text for bug reports

**Target state:**

1. **Multi-path toggle** — When WASM is available, show "Path 1 of N ◀ ▶" navigator
   above the chain. Each path is a distinct `RootPathStep[]` from the WASM
   `all_gc_root_paths()` call. Default = shortest path (current behavior).

2. **Clickable field edges** — Clicking `.table` on a `HashMap → HashMap$Node[]`
   edge fires `fireInspect({ kind: "field-scan", cls: "java.util.HashMap",
   fieldName: "table" })` → opens HeapInspector showing all instances of HashMap
   whose `.table` field points to this class.

3. **Clickable root type** — When `root_type_label` contains "Java Frame" or
   "Thread", clicking navigates to `#threads` + highlights the relevant thread.
   When "System Class", navigates to the histogram row for the root class.

4. **Progressive expand for DomSubtreeSvg** — Instead of pruning to depth 1
   when >25 nodes, show depth 1 with "▶ Expand N children" buttons on each leaf
   that has children. Clicking expands that subtree one more level.

5. **Copy chain as text** — "⎘ Copy chain" button above the SVG that copies:
   ```
   ① com.app.UserCache         124.8 MB  ← .cache
   ② com.app.AppServlet          2.1 MB  ← .servlet
   ③ Thread "http-8080-1"        0.5 MB  ← Java Frame (GC Root)
   ```

6. **Chain in every class tooltip** — The top-20 histogram classes already have
   `root_path` in the report JSON. The detail popover (X2) for these classes
   shows a mini chain inline — no need to expand the row.

**Effort:** ~300 LOC for multi-path + clickable edges + clickable root + progressive
expand + copy. This is the single highest-value interaction improvement.

---

### X6. "Why Is This Alive?" One-Click Flow 🟢

**Current state:** To understand why a class is alive, users must: (1) find it in
the histogram, (2) expand the row to see root_path (if available), (3) if not
available, open in Inspector, (4) navigate to the Instance tab, (5) click "Paths
to GC Root". This is 5+ clicks for the most fundamental question in heap analysis.

**Target state:** One click. Every class name in every table/chart has a
🌳 button (or context menu item) that opens a focused "Why Alive?" popover:

```
┌─────────────────────────────────────────────┐
│ Why is HashMap$Node[] alive?          ✕     │
├─────────────────────────────────────────────┤
│ GC Root Chain (shortest path):              │
│                                             │
│  ① HashMap$Node[]      124.8 MB retained    │
│     ↑ .table                                │
│  ② java.util.HashMap    126.9 MB retained   │
│     ↑ .cache                                │
│  ③ com.app.UserCache    127.1 MB retained   │
│     ↑ <local>                               │
│  ④ Thread "http-8080"   GC Root: Java Frame │
│                                             │
│ Path 1 of 3  [◀] [▶]                       │
├─────────────────────────────────────────────┤
│ Root type: Java Frame (thread-local ref)    │
│ Fix complexity: Medium                      │
│ Fix pattern: Ensure ThreadLocal.remove()    │
│              is called in finally block     │
├─────────────────────────────────────────────┤
│ [⬡ Explore Instance] [⌞ Dominator Tree]    │
│ [⎘ Copy Chain]                              │
└─────────────────────────────────────────────┘
```

For the top-20 classes, `root_path` is already in the report JSON — zero WASM
needed. For other classes, falls back to WASM `gc_root_path()` or shows
"Enable --ref-paths to see GC root chains for all classes".

**Effort:** ~200 LOC (popover component reusing RootPathChain rendering).

---

### X7. Per-Idea Interaction Annotations

Below, every idea in the catalog (A1–N4) should include an **Interactions** row
in its table specifying:

| Field | Content |
|-------|---------|
| **Hover** | What the tooltip shows (use X1 standard) |
| **Click** | What happens on primary click (drill-down, navigation) |
| **Right-click** | Context menu actions available (use X3 standard) |
| **Cross-links** | Which other sections this view links to |
| **Popover** | What the detail popover (X2) shows for this element |

This replaces the vague "click navigates to..." notes in the current draft with
precise, implementable interaction specs. Each idea's Interactions row is defined
inline with the idea.

---

### Interaction Patterns by View Type

#### Treemaps (FlatTreemap, ZoomableTreemap)

| Action | Behavior |
|--------|----------|
| Hover tile | Rich tooltip: class/name, retained (exact), shallow, instances, % of parent, % of total |
| Click tile (FlatTreemap) | Open detail popover with full class info + GC root chain |
| Click tile (ZoomableTreemap) | Drill into children (existing), BUT: shift-click → detail popover |
| Right-click tile | Context menu (X3): Show in Histogram, Show Holders, Explore Instance, Copy |
| Click breadcrumb | Navigate back to parent (existing — keep) |

**Migration from native `title`:** Replace all `title={...}` in `FlatTreemap`
(line 140) with `<Tooltip>` component wrapping each div.

#### Tables (StdTable + DataTable)

| Action | Behavior |
|--------|----------|
| Hover row | Subtle highlight (existing) |
| Click class name | Open detail popover (X2) |
| Click action buttons | Existing: ⎘ Copy, ⬡ Inspect, ⌗ OQL, ⬡≡ Instances, ⬡↗ Explore, ⌞ Dominator |
| Right-click row | Context menu (X3) |
| Expand row (▶) | Show root_path chain (existing for top-20 histogram). **NEW:** expand for ALL rows that have `root_path`, plus "Load chain..." button for rows without (triggers WASM) |
| Hover numeric cell | Rich tooltip with exact bytes + context |

#### Sankey Diagrams

| Action | Behavior |
|--------|----------|
| Hover node | Rich tooltip: class name, retained flow (exact), inbound/outbound edge count, top holder class |
| Click node | Open detail popover with class info + mini retention chain |
| Hover link | Rich tooltip: source → dest, weight (exact), % of total flow |
| Click link | Highlight the specific ImmDomPair in the Dominator Analysis table |
| Right-click node | Context menu (X3) |

#### Scatter Plots

| Action | Behavior |
|--------|----------|
| Hover point | Rich tooltip: class name, X value (exact), Y value (exact), instances, outlier flag |
| Click point | Open detail popover with full class info |
| Right-click point | Context menu (X3) |
| Drag to select | Lasso/box select → filter table below to selected points |
| Hover axis | Tooltip explaining what the axis measures |

#### Bar Charts (VBar, HBar, StackedBar)

| Action | Behavior |
|--------|----------|
| Hover bar | Rich tooltip: label, value (exact), % of total, rank |
| Click bar | Navigate to related section or filter related data |
| Right-click bar | Context menu (X3) when bar represents a class |

#### GC Root Path Chains (RootPathChain, DomSubtreeSvg)

| Action | Behavior |
|--------|----------|
| Hover step box | Rich tooltip (X1): class, retained, % of suspect, field edge, step N/M, GC root type |
| Click step box | Navigate to Object Graph Explorer at that object (existing) |
| Click field edge label | `fireInspect({ kind: "field-scan" })` — find all objects held via this field |
| Click GC root type | Navigate to relevant Thread (Java Frame) or ClassLoader (System Class) |
| Click "⎘ Copy chain" | Copy text representation of full chain |
| Click "Path N of M ▶" | Switch to next GC root path (WASM required) |
| Click "▶ Expand" (DomSubtreeSvg) | Progressively expand one more level of the dominator subtree |

---

### Shared Infrastructure Components

These components are shared across all ideas and should be built first:

| Component | LOC | Priority | Blocks |
|-----------|:---:|:--------:|--------|
| Rich tooltip migration (X1) | 200 | **P0** | Everything — inconsistent tooltips break the expert experience |
| Detail popover (X2) | 400 | **P0** | Per-class deep-dive from any view |
| Context menu (X3) | 600 | **P1** | Right-click exploration everywhere |
| Cross-section highlight (X4) | 400 | **P1** | Multi-section class investigation |
| Dominator chain improvements (X5) | 300 | **P0** | Multi-path, clickable edges, progressive expand |
| "Why Is This Alive?" flow (X6) | 200 | **P0** | One-click root-cause for any class |
| **Total infrastructure** | **2,100** | | |

These 2,100 LOC of infrastructure multiply the value of every existing and
proposed visualization. They should be Sprint 0 alongside the Rust data
enrichment — before any new chart is built.


## Idea Catalog

### Verdicts explained

| Symbol | Meaning |
|--------|---------|
| 🟢 **Implement** | High value/effort ratio. Clear user benefit, data ready or nearly ready. |
| 🟡 **Consider** | Valuable but lower priority — niche audience, high effort, or needs opt-in flag. |
| ⚪ **Defer** | Interesting but marginal. Implement only if the category is already well-served. |

---

### A — Dominator & Ownership Structure

*These views answer: "WHO holds WHAT, and what is the shape of the ownership tree?"*

#### A1. Dominator Sunburst 🟢

| | |
|-|-|
| **Question** | Is this a one-big-object problem or a wide fan of many holders? |
| **Data** | `Suspect.dominator_tree: DomTreeNode` (always-on, recursive) |
| **Library** | `d3-hierarchy partition()` + manual SVG arc paths — **zero new packages** |
| **Where** | Leak Suspects section, alongside existing DAG. Or as a 3rd mode toggle in `ZoomableTreemap`. |
| **Demo** | [`docs/visualization-demos.html#demo-sunburst`](docs/visualization-demos.html#demo-sunburst) |

**How it works with existing libraries:**

`d3-hierarchy` is already installed. The sunburst layout is:
```typescript
const root = d3.hierarchy(domTree, n => n.children)
  .sum(n => n.children?.length ? 0 : n.retained);
d3.partition().size([2 * Math.PI, radius])(root);
```
Each node gets `x0`, `x1` (angles), `y0`, `y1` (radii). The SVG arc path is ~15
lines of trigonometry — no `d3-shape` or `d3-arc` needed:
```typescript
function arcPath(x0, x1, y0, y1) {
  const largeArc = (x1 - x0) > Math.PI ? 1 : 0;
  return `M${y0*Math.cos(x0)},${y0*Math.sin(x0)}
    A${y0},${y0},0,${largeArc},1,${y0*Math.cos(x1)},${y0*Math.sin(x1)}
    L${y1*Math.cos(x1)},${y1*Math.sin(x1)}
    A${y1},${y1},0,${largeArc},0,${y1*Math.cos(x0)},${y1*Math.sin(x0)}Z`;
}
```
The demo proves this approach renders correctly with click-to-drill-in.

**Pros:**
- Shows the *shape* of dominator ownership at a glance — deep chain vs. wide fan vs. balanced tree
- JProfiler added this in v11.1 (2022) and it's now their most-used heap view
- Data is already in the report with zero backend changes
- ~60 LOC total (layout + SVG rendering + click handler)

**Cons:**
- Sunbursts become unreadable beyond 6 rings depth — must truncate
- On very flat heaps (many small dominators), the chart becomes a featureless donut
- Drill-down needs breadcrumb state management (~20 extra LOC)

**Performance:** SVG-based; ~500 arcs max after depth truncation. ~0.5 ms layout, ~2 ms render.
JSON unchanged.

**Memory impact:** ~40 KB SVG DOM for a full 500-arc sunburst. Negligible.

**Interactions:**

- **Hover arc:** Rich tooltip — class name, retained (exact bytes), % of suspect's
  total, depth/ring level, children count, parent dominator class. Action hint:
  "Click to zoom in. Shift-click for detail."
- **Click arc:** Zoom into that ring subset (drill-down, existing d3 partition behavior).
  Breadcrumb trail updates: `Root › HashMap › Entry[] › byte[]`. Each breadcrumb
  segment is clickable (zoom back) and has a `⬡` PivotBtn.
- **Shift-click arc:** Detail Popover (X2) — full class info + mini GC root chain
  sourced from `Suspect.root_path` (the suspect's path includes all ancestors).
  Action buttons: `[⬡ Inspect]` `[⬡≡ Instances]` `[⌗ OQL]` `[⎘ Copy]`.
- **Right-click arc:** Context menu — Show in Histogram, Show Holders, Explore Instance,
  Show in Dominator Tree, Copy Class Name, Copy OQL.
- **🌳 button** (in tooltip or breadcrumb): "Why Alive?" popover — because this IS the
  dominator tree, the path from sunburst root to the selected arc IS the dominator chain.
  Render it as a `RootPathChain` with field edges.
- **Cross-links:** Clicking any class name fires `highlight-class` to all sections.
  Drilling into a depth level dispatches `highlight-class` for the newly-focused class.

**Effort:** ~80 LOC (60 chart + 20 breadcrumbs/state).

---

#### A2. Who-Holds-Whom Heatmap Matrix 🟢

| | |
|-|-|
| **Question** | Which class pairs have the strongest dominator relationships, and how do I trace each pair back to a GC root? |
| **Data** | `DominatorAnalysis.immediate_dominators.pairs: ImmDomPair[]` (always-on, up to 20K pairs), `ImmediateDominators.rows: ImmediateDominatorRow[]`, `HistRow[]` (for root_path of top classes) |
| **Library** | Pure HTML `<table>` + CSS `rgba()` backgrounds — **zero library needed** |
| **Where** | Dominator Analysis section, above the existing pair table. |
| **Demo** | [`docs/visualization-demos.html#demo-heatmap`](docs/visualization-demos.html#demo-heatmap) |

**How it works:**

Extract top-N dominator and dominated classes from `ImmDomPair[]`, build a 2D
matrix, color each cell with `rgba(37, 99, 235, opacity)` where opacity ∝
`sqrt(retained / max_retained)`. Square root scaling prevents the top-1 cell
from dominating the color range.

```typescript
const opacity = Math.max(0.08, Math.sqrt(pair.retained / maxRetained));
cell.style.background = `rgba(37, 99, 235, ${opacity})`; 
```

**Toolbar (above the heatmap):**

| Control | What it does |
|---------|-------------|
| **Size slider** (5–30) | Controls the matrix dimension — `5×5` for overview, `20×20` for deep analysis, `30×30` for full picture |
| **Category filter** | `[All] [App→App] [App→JDK] [JDK→App] [JDK→JDK] [Framework→*]` — filter by dominator/dominated category pair. Default: All. The `App→JDK` filter (e.g., your code retaining `byte[]`) is usually the most interesting. |
| **Sort axis** | `[By retained ▼] [By pair count] [Alphabetical]` — controls how rows/columns are ordered |
| **Text search** | Type a class name → matching row AND column highlight with accent border; non-matching dim to 20% opacity. Typing `HashMap` highlights HashMap as both dominator (column) and dominated (row). |
| **Min retained threshold** | Slider: "Hide cells below N KB" — reduces noise in large matrices |

**Rich tooltip on cell hover** (via `<Tooltip>` component, not native `title`):

```
┌──────────────────────────────────────────────────┐
│ HashMap$Node[]  ◄──dominated by──  HashMap       │  ← TipHeader
├──────────────────────────────────────────────────┤
│ Dominated retained    84.2 MB  (exact bytes)     │  ← primary metric
│ Dominated shallow      2.3 MB  (exact bytes)     │
│ Pair count             12,847  instances          │  ← how many individual pairs
│ % of total heap        23.1%                     │
│ Dominator category     framework                 │  ← from ImmDomPair.dominator_category
│ Dominated category     jdk                       │  ← from ImmDomPair.dominated_category
│ Top app dominator      com.app.UserCache         │  ← from ImmDomPair.top_app_dominator
│ Retention multiplier   36.6×                     │  ← retained / shallow
├──────────────────────────────────────────────────┤
│ Click cell for retention path ▶                  │
│ Right-click for more actions                     │
└──────────────────────────────────────────────────┘
```

The `top_app_dominator` field (from `ImmDomPair.top_app_dominator`) is critical — it
answers "yes, HashMap dominates Node[], but who dominates the HashMap? → your app code".
This cuts through the JDK noise and points directly at the application-level fix target.

**Click a cell** → inline expansion below the heatmap showing:

1. **Pair detail card:** All tooltip fields in a readable layout, plus action buttons
   on both dominator and dominated class names (CopyBtn, PivotBtn, OqlBtn, ListObjectsBtn).

2. **GC root chain** (when available): If either the dominator or dominated class is
   in the top-20 histogram (where `HistRow.root_path` is populated), render a
   `RootPathChain` showing how this relationship connects to a GC root. The chain
   answers: "HashMap → UserCache.cacheMap → static field AppContext.instance → GC Root".
   
   If neither class has a root_path, show: "Explore in Object Graph →" button that
   opens the WASM inspector to compute the path on demand (if available), or
   "Run OQL →" with a pre-filled query: `SELECT * FROM HashMap WHERE @dominatedRetainedSize > 1000000`.

3. **Top-app-dominator chain:** When `top_app_dominator` is present and different from
   the dominator, show a mini 3-step chain: `[App Class] → [Framework/JDK Dominator] → [Dominated]`
   with retained bytes at each step. This answers "what's MY code's role in this?"

4. **"Other pairs by this dominator"** collapsible: Show the other dominated classes
   held by the same dominator, sorted by retained. This answers "HashMap dominates
   Node[], but what ELSE does it dominate?" — helps determine if the dominator is a
   systemic bottleneck or a one-off.

**Right-click a cell** → context menu:
- "Show Dominator in Histogram" → cross-section highlight of dominator class
- "Show Dominated in Histogram" → cross-section highlight of dominated class
- "Show Top App Dominator" → cross-section highlight of `top_app_dominator`
- "Explore Dominator Instances" → `fireInspect({ kind: "instances", cls: dominator_class })`
- "Copy Pair as TSV" → `"dominator\tdominated\tretained\tpair_count"`
- "Copy OQL" → `SELECT * FROM dominated_class WHERE @immediateDominator.@class.name = "dominator_class"`

**Row/column header interactions:**

- **Hover a class label** (row or column header): Highlight the entire row or column
  with a subtle accent background — shows all pairs involving that class at once.
  Tooltip shows: total retained as dominator, total retained as dominated, instance
  count, category.

- **Click a class label:** Open Detail Popover (X2) for that class with: full metrics,
  GC root chain, action buttons, dominated-classes summary.

**Pros:**
- Makes cross-retention anomalies visible at a glance
- The filter bar makes a 20×20 matrix of 400 cells navigable — `App→JDK` filter
  immediately isolates "my code retaining JDK collections"
- `top_app_dominator` in the tooltip is the key expert insight — it cuts through
  framework/JDK indirection to show the application-level root cause
- Click → GC root chain means users never hit a dead end — every cell leads to
  the code path responsible

**Cons:**
- Top 30×30 matrix = 900 cells. DOM performance is fine; visual density may be high.
  Mitigate with the size slider (default to 15×15) and threshold filter.
- Long Java class names require truncation in axis labels — full name in tooltip
- The "Category filter" interaction requires `ImmDomPair.dominator_category` and
  `dominated_category` fields, which are present in newer reports but absent in older ones

**Performance:** 400–900 `<td>` elements + event listeners. ~2 ms render. JSON unchanged.

**Memory impact:** ~150 KB DOM for a 20×20 matrix with tooltips. Negligible.

**Effort:** ~150 LOC (up from ~50 LOC in previous draft — the extra ~100 LOC buys
the filter bar, rich tooltips, click expansion, and context menu).

---

#### A3. Big Drop Stepdown Waterfall 🟢

| | |
|-|-|
| **Question** | Where does retained heap concentrate into an object without flowing to any single large child? |
| **Data** | `DominatorAnalysis.big_drops.rows: BigDropRow[]` (always-on) |
| **Library** | Pure SVG `<rect>` elements |
| **Where** | Dominator Analysis section, as a chart above the existing Big Drops table. |
| **Demo** | [`docs/visualization-demos.html#demo-waterfall`](docs/visualization-demos.html#demo-waterfall) |

**How it works:**

Each `BigDropRow` has `retained`, `drop_bytes`, `largest_child_retained`. Draw
two adjacent bars per row: blue for `largest_child_retained` (flows through),
red for `drop_bytes` (absorbed by this object). Sort by `drop_bytes` descending.

**Pros:**
- Big drops are the most unique dominator insight — they're where memory "teleports" and accumulates
- `BigDropRow` already has all three fields — zero backend changes
- Visually conveys the *magnitude* of each drop, which the table doesn't

**Cons:**
- Waterfall charts are less familiar than bar charts — needs an explanatory legend
- Small number of rows (typically 5–20) — the chart may look sparse
- Red/blue color coding needs to work in both light and dark themes

**Performance:** 5–20 SVG `<rect>` pairs. Trivial. JSON unchanged.

**Memory impact:** <10 KB SVG. Negligible.

**Interactions:**

- **Hover a bar pair:** Rich tooltip — class name, `retained` (exact), `drop_bytes`
  (exact), `largest_child_retained` (exact), absorption ratio (`drop_bytes/retained`
  as %), `child_count`, rank in the big-drops list. Accent color on the "absorbed"
  portion.
- **Click a bar:** Detail Popover — full class info + `[⌞ Show in Dominator Tree]`
  button (opens the same dominator chain explorer for this object). If the drop
  object's class is in the top-20 histogram, show its `RootPathChain`. Note:
  "This object absorbs N bytes that don't flow to any single child — it's an
  accumulation point (likely a collection or buffer)."
- **Right-click:** Context menu — Show in Histogram, Show Holders, Explore Instance,
  Show in Dominator Tree, Copy OQL.
- **Cross-link to table:** Clicking a waterfall bar highlights the corresponding row
  in the existing Big Drops table below (scroll + accent flash). Clicking a table
  row highlights the corresponding bar (bidirectional linking).

**Effort:** ~70 LOC.

---

#### A4. Immediate Dominator Chord Diagram ⚪

| | |
|-|-|
| **Question** | What is the overall topology of class-level retention relationships? |
| **Data** | `ImmDomPair[]` (same as A2) |
| **Library** | Would require `d3-chord` (~2.4 KB) — but see verdict |

**Verdict: DEFER.** The heatmap (#A2) serves the same purpose with better
readability for rectangular (non-square) data. Chord diagrams are beautiful but
hard to label with long Java class names, and the heatmap's hover behavior is
more intuitive. If the heatmap proves insufficient, `d3-chord` is tiny and can
be added later.

---

#### A5. Self-Referential Chain Map 🟢

| | |
|-|-|
| **Question** | Which classes form deep recursive chains (LinkedList nodes, tree nodes), and how long are the chains? |
| **Data** | `Report.self_referential_classes: SelfReferentialClassRow[]` (always-on) |
| **Library** | `VBar` (existing) + inline table |
| **Where** | Dominator Analysis section. |

**Pros:**
- `self_referential_classes` is **completely unvisualized** today
- Recursive chains are a common source of deep GC pauses and stack overflow risks
- `nested_pct` is a unique metric — "what % of Node instances are nested inside another Node"

**Cons:**
- Small dataset (typically 1–5 classes). A compact table + bar suffices; full chart is overkill.

**Performance:** 1–5 bars + table rows. Trivial. JSON unchanged.

**Memory impact:** Negligible.

**Interactions:**

- **Hover bar:** Rich tooltip — class name, `nested_pct` (exact %, with plain English:
  "X% of all N instances are nested inside another instance of the same class"),
  `retained` (exact), chain depth range, % of heap.
- **Click bar:** Detail Popover — class info + GC root chain + note: "Deep recursive
  chains risk StackOverflowError during GC mark phase. Consider flattening to an
  iterative structure (ArrayList) or limiting depth."
- **Right-click:** Context menu — Show in Histogram, Explore Instance, Copy OQL.
- **Cross-link:** Fires `highlight-class` for the chain class. For self-referential
  classes (e.g., `LinkedList$Node`), the "Why Alive?" chain will show the chain
  collapsing — display with a "…repeated N more times" truncation indicator.

**Effort:** ~50 LOC.

---

#### A6. Object-Level Reference Cycles / SCC Explorer 🟡

| | |
| --- | --- |
| **Question** | Which concrete objects form cycles, and which external roots or fields keep those cyclic structures reachable? |
| **Data** | New SCC analysis over a complete object-reference graph with an explicit reference-strength policy (D-20); A5 remains class-level nesting analysis. |
| **Library** | Existing Cytoscape for a selected component; table + inline bars for the overview |
| **Where** | Object Graph and Dominator Analysis, cross-linked from suspects. |

**How it works:**

Run an iterative Tarjan/Kosaraju-style strongly-connected-component (SCC) pass;
collapse each multi-object component, or a single object with a self-edge, into
one node. The resulting condensation graph is a DAG of **reference connectivity**,
not a dominator tree. Rank components by member shallow bytes, object count,
and significant external entry paths. Example: a large parent↔child entity graph
or a cache's doubly-linked entries retained through one static registry.

- Expand a component to inspect member classes and field edges; show incoming
  edges from outside the component and paths back to GC roots.
- Separate member shallow bytes from a deduplicated downstream footprint. Summing
  retained sizes of every member would count the same dominated objects repeatedly.
- Offer "simulate removing the external entry edge(s)" through B2, rather than
  recommending arbitrary cuts to internal links.

**Why it helps:** A5's same-class dominator nesting cannot explain concrete cycles
that span several classes. This view reveals the cyclic topology while directing
the user to the *external* lifetime owner that matters.

**Critical limit:** Tracing Java GC collects unreachable cycles. A cycle by itself
is **not a leak** and does not require breaking to be collected; a GC-root path
or another reachable owner is still required. Linked lists and bidirectional
domain graphs are often intentional. Exclude weak/soft/phantom referent edges
from a strong-reference view, or clearly expose the selected policy.

**Performance / memory:** O(V + E) graph work with O(V) scratch state. Full-heap
SCC analysis can be expensive on very large dumps; keep it opt-in and budgeted.
A bounded captured graph can split true components, so label such results
"within captured graph" and never present them as exhaustive.

**Effort:** Large opt-in backend work plus a medium overview/detail UI. No new
frontend chart package. See D-20.

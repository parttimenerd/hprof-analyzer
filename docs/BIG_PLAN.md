# hprof-analyzer: BIG_PLAN — Comprehensive Views & Visualisations

> Compiled 2026-07-29. Supersedes `docs/views-plan.md` and `docs/new-features-masterplan.md`.
> Covers every proposed view: value, implementation cost, generation cost, report-size cost,
> RSS impact, ASCII-art mockup, flag recommendations, library notes, and honest critique.
> Research sources: Eclipse MAT source, JProfiler, YourKit, HeapHero, Chrome DevTools,
> Firefox DevTools, JXRay, VisualVM/JOverflow, dotMemory, async-profiler, Cork POPL 2007,
> HeapViz ISMM 2011, production OOM case studies, HN/Reddit/StackOverflow.

---

## 0. Constraints & Library Budget

### Bundle budget

Current: **712.3 KB** minified. The 720 KB cap is a historical artifact (raised from 660 KB when react-data-table was added) — it is **not a hard constraint**. This is a self-contained diagnostic tool embedded in a single HTML file; the gzip-compressed payload is 214 KB. Adding 50–100 KB for genuinely useful libraries is fine. The rule is *value per byte*, not a fixed ceiling.

| Library | Bundle cost (raw minified) | Status | Notes |
|---------|---------------------------|--------|-------|
| d3-hierarchy | ~28 KB | ✓ in use | `partition()`, `treemap()`, `hierarchy()`, `tree()` all available |
| d3-dag | ~90 KB | ✓ in use | sugiyama layout for DomSubtreeSvg — could save ~60 KB by switching to `tree()` for pure trees, but not worth the churn |
| chart.js | ~350 KB | ✓ in use | Dominant cost. Only Pie + Bar used. Not worth replacing. |
| react-data-table-component | ~90 KB | ✓ in use | StdTable<T> wrapper |
| react + react-dom | ~100 KB | ✓ in use | Unavoidable |
| **d3-sankey** | **~7.9 KB** | **add** | Pulls d3-shape + d3-path as deps; 7.9 KB total raw minified. Use for V5, V13, V18 sankeys. |
| **d3-force** | **~5.5 KB** | **add** | Live physics simulation in browser for V13 force-graph toggle. Enables drag, zoom, click-to-highlight. Pre-computing in Rust would save 5.5 KB but lose all interactivity. |
| d3-shape arc() | ~0 KB extra | ✓ via d3-sankey | d3-sankey pulls d3-shape as a dep; arc() is free once sankey is added |
| TanStack Table | ~14 KB | skip | react-data-table-component covers the use cases |
| Cytoscape.js | ~131 KB | skip | Way too large for the value; pre-computed SVG is sufficient |
| HTML `<details>` | 0 KB | ✓ native | All inline tree expansion |
| Pure CSS sparklines | 0 KB | ✓ native | CSS `linear-gradient` bar charts |

**Decision**: add d3-sankey (~7.9 KB) and d3-force (~5.5 KB). Together ~13.4 KB. d3-sankey unlocks V5, V13, V18 sankeys and brings d3-shape with it (covers V17 arc at no extra cost). d3-force enables interactive force-graph mode for V13 with drag/zoom/highlight — pre-computing layout in Rust would save the 5.5 KB but make the graph static and much less useful.

**Pre-computed layouts**: For V13 the browser runs d3-force live — drag nodes, zoom, click to highlight connected edges. No Rust pre-computation needed (and Rust pre-computation would remove all interactivity). For the dominator subtree SVG (DomSubtreeSvg), sugiyama layout via d3-dag runs once on render, which is fine since those trees are small (≤25 nodes).

---

## 1. V1 — Executive Summary Card

### Value

**★★★ Highest value per line of code.**
Every commercial tool opens with a summary. HeapHero's "At a Glance" header is the first thing users mention in reviews. The question "what is this heap dump telling me?" takes 5–15 minutes to answer by reading the full report; this card answers it in 10 seconds.

The Netflix case study, the Hibernate session case study, and every r/java OOM post share the same pattern: the user wants the one-liner verdict before diving into tables.

Critique: The only risk is oversimplification — a bad heuristic ("top suspect is HashMap") misfires on unrelated reports. The card must be carefully worded ("largest retained contributor") not "leak suspect". Triage signals already exist and are well-calibrated.

### Implementation cost: S (2–3 hours, frontend only)

Pure composition of existing data fields. No new backend. No new data structures.

### Generation cost: none

Zero CPU at analysis time. All data already present.

### Report size: none

No JSON additions. The card reads from existing fields.

### RSS impact: none

### ASCII art

```
┌────────────────────────────────────────────────────────────────────────┐
│  ⬟ HEAP SUMMARY                                       2026-07-15 14:32 │
│  myapp-production.hprof  ·  OpenJDK 17.0.9  ·  JVM heap: 4.0 GB       │
│                                                                         │
│  Reachable: 3.81 GB (95%)   Garbage: 198 MB (5%)   Wasted: 340 MB     │
│                                                                         │
│  ● LEAK RISK   ● HIGH GC    ○ METASPACE OK   ● OFF-HEAP: 1.2 GB       │
│                                                                         │
│  Top suspect: java.util.HashMap holds 1.24 GB (30.7%)                  │
│               kept alive by Thread "http-exec-3"                        │
│                                                                         │
│  ⚠ 47 stale ThreadLocal entries (~180 MB estimated)                    │
│  ⚠ DirectByteBuffer off-heap: 1.2 GB across 3,400 instances            │
└────────────────────────────────────────────────────────────────────────┘
```

### Flag: always-on

### Libraries: none

---

## 2. V2 — Thread Retention Ranked Table

### Value

**★★★ Direct answer to "which thread is leaking?"**

The Netflix production OOM (case study #3) was diagnosed by sorting threads by retained heap: one thread held 85% of all reachable memory. Without this table, users scan all threads manually. MAT has no direct equivalent — you must run an OQL query (`SELECT * FROM java.lang.Thread`), sort manually, then cross-reference. YourKit has this as a first-class view and it's one of their most-used features.

Critique: On heaps dominated by a single large data structure, all threads will show low retained heap and the table will be uninformative. Worth including regardless — a low retained heap per thread is also a signal (the leak is not thread-local).

### Implementation cost: S (1–2 hours, frontend only)

Sort `threads[]` by `retained` descending. Add as a table at the top of the Threads section. No backend changes. The `threads[i].retained` field already exists.

### Generation cost: none

### Report size: none

### RSS impact: none

### ASCII art

```
┌──────────────────────────────────────────────────────────────────────────────┐
│  THREADS BY RETAINED HEAP                                                    │
├──────────────────────────────────┬──────────┬──────────┬─────────┬───────────┤
│  Thread name                     │  State   │ Retained │ Locals  │ Top class │
├──────────────────────────────────┼──────────┼──────────┼─────────┼───────────┤
│  http-exec-3                     │ RUNNABLE │  1.24 GB │   4,218 │ HashMap   │
│  http-exec-1                     │ BLOCKED  │  380 MB  │   1,102 │ byte[]    │
│  pool-2-thread-12                │ WAITING  │   88 MB  │     420 │ String    │
│  RMI TCP Connection(1)           │ WAITING  │    4 MB  │      18 │ Object[]  │
│  Finalizer                       │ WAITING  │    1 MB  │       3 │ Object    │
│  … (23 more threads, < 1 MB each)│          │          │         │           │
└──────────────────────────────────┴──────────┴──────────┴─────────┴───────────┘
```

### Flag: always-on

### Libraries: react-data-table-component (already in bundle)

---

## 3. V3 — Reference Graph Explorer (outbound walk)

### Value

**★★★ The most-requested missing feature in heap analysis tools.**

StackOverflow's top heap-dump question (2,847 votes): "How do I find what's holding a reference to an object in Java?" HN 2023: "The killer feature of MAT is clicking through the reference graph — but MAT's UX for this is terrible, you lose track of where you are after 3 clicks."

This is the outbound-reference navigator: start at a big object, click through its fields, navigate back. Chrome DevTools' "Containment" view is the gold standard UX. The key additions over MAT: explicit Back/Forward + breadcrumb trail, and URL-hash state so users can bookmark and share specific object views.

JXRay confirmed this architecture works in static HTML: flat JSON lookup table, browser navigates lazily. No server round-trips.

**What we explicitly do not build**: a full retainer (inbound) list per object. The inbound CSR is consumed during analysis and is not available at report time. The immediate dominator (`idom`) covers the primary retainer for significant objects. If users need multi-retainer analysis they run an OQL query (`SELECT * FROM ... WHERE dominatedBy(...)` equivalent). This constraint should be stated plainly in the UI: "Showing immediate dominator only — for full retainer list, use the OQL tab."

**Critical design clarification on BFS seeding**: roots are determined by `g.idom[i] == vroot` (objects with no dominator = top-level retained). BFS for the `edges` map uses `cap.captured` (objects in the shallow-heap capture). These are different sets. An object can be a dominator root but have `edges_unknown: true` if it wasn't captured (not in top-10,000 shallow). The `dom_children` walk covers all BFS-reachable sig nodes regardless of capture. Both maps are populated independently — a node can appear in `dom_children` without having an `edges` entry.

### Implementation cost: M (1–2 days backend + React)

- Backend: `capture_obj_graph_edges()` ✓ already implemented. `build_obj_graph_flat()` needed in `report/build.rs`. New structs in `report/model.rs`.
- Frontend: `ObjectGraphExplorer` component. Navigation via URL hash (`#explore/NODE_ID`) — free browser Back/Forward, no custom history stack needed. Breadcrumb derived from `window.location.hash` change events.

### Generation cost: low

- Capture (pre-inbound): O(top_n × edge_cap) = 10,000 × 100 = 1,000,000 HashMap insertions. ~10 ms.
- BFS at build_model: O(sig_nodes × avg_children). For typical 1–4 GB heaps: ~10 ms.

### Report size

| Heap | Sig nodes | JSON (raw) | Compressed |
|------|-----------|-----------|-----------|
| 1 GB typical | ~200 | ~480 KB | ~50 KB |
| 4 GB large | ~800 | ~2 MB | ~200 KB |
| Worst case (200k cap) | 200,000 | ~40 MB | ~2 MB |

The worst-case raw figure is ~40 MB (200k nodes × ~200 bytes avg per node including sparse edges post-significance-filter), compressing to ~2 MB. The "50 MB" figure in earlier drafts assumed every node had 50 edges — impossible after significance filtering prunes most children.

### RSS impact

- During capture: `ObjGraphCapture` HashMap holds ~30 MB peak (10,000 nodes × 100 edges × ~30 bytes per HashMap entry). Freed after `build_model`.
- During BFS build: O(sig_nodes) working set, < 10 MB.
- Net RSS increase during analysis: **~40 MB peak, freed immediately after build_model**.

### ASCII art

```
┌──────────────────────────────────────────────────────────────────────────┐
│  Reference Graph Explorer                       ← Back  Forward →  ⌂    │
│  HashMap#1234  ›  .table Entry[]#5678  ›  [0] Entry#9012                 │
│  (breadcrumb: field·class pairs — click any to jump back)                │
├──────────────────────────────────┬───────────────────────────────────────┤
│  OUTBOUND REFERENCES             │  DETAILS                              │
│  of: Entry[]#5678                │  java.util.HashMap$Entry[]            │
│  shallow 16 MB / retained 318 MB │  index:    #5678                      │
│                                  │  shallow:  16 MB                      │
│  Filter by class… [field name ▾] │  retained: 318 MB  (24.8%)            │
│  ┌──────┬──────────────┬────┬────┤  idom:     HashMap#1234   [nav →]     │
│  │  →   │ .field·class │shl │ret │                                       │
│  ├──────┼──────────────┼────┼────┤  DOMINATOR CHILDREN (from dom tree)   │
│  │ [→]  │[0] Entry     │ 72B│18MB│  ┌─────────────────────────────────┐ │
│  │ [→]  │[1] Entry     │ 72B│17MB│  │  [into▶] Entry ×43200  (290 MB) │ │
│  │ [→]  │[2] Entry     │ 72B│16MB│  │  — (9 classes < 1 MB)           │ │
│  │  —   │[3] null      │  — │  — │  └─────────────────────────────────┘ │
│  │      │ 49 of 8192   │    │    │  [→ Open in Dominator Explorer]       │
│  │      │ [next 50 ▶]  │    │    │                                       │
│  └──────┴──────────────┴────┴────┴───────────────────────────────────────┤
│  Objects < 1 MB shown as leaves (no [→] button). Field names need        │
│  --ref-paths. Retainers: idom only — full list via OQL tab.              │
└──────────────────────────────────────────────────────────────────────────┘
```

**Key UX details revised from earlier design:**

1. **URL hash navigation** (`#explore/1234`): replaces the hand-rolled 50-step history stack. Browser Back/Forward work natively. Users can link colleagues to a specific object. No React state management overhead.

2. **Breadcrumb encodes the path taken**: each segment is `.fieldName ClassName#id` (how you arrived, not just where you are). For arrays: `[0] ClassName#id`. For unnamed edges (no `--ref-paths`): just `ClassName#id`.

3. **Pagination instead of "show all"**: rows capped at 50, "next 50 ▶" button for objects with many children. "Show all 8192" would hang the browser for large arrays. Pagination keeps the table responsive.

4. **Filter by field name** (dropdown alongside class filter): for a `byte[]`-heavy object, filtering by `.value` field immediately surfaces the relevant children without scrolling through hundreds of unrelated fields.

5. **Right-panel dominator children** are rendered from `dom_children` in `ObjGraphFlat` as a compact table (not `DomSubtreeSvg`), because `DomSubtreeSvg` requires a pre-built `DomTreeNode` tree which only exists for suspects. The compact table is: class, count, retained, `[into▶]` nav. A "Open in Dominator Explorer" link switches to V4 for the same object.

6. **`edges_unknown` handling**: when a node is navigable in the dom tree but wasn't in the capture set, the left panel shows "⚠ Outbound references not captured for this object (not in top-10,000 by shallow heap). Use OQL for full reference list."

7. **Cross-connection badge** (`⟲ shared`): when a child's `idom` field (stored on every `ObjGraphFlatNode`) differs from the current object's ID, the child is *shared* — it is retained by a different primary retainer and appears here only as an outbound reference. Render a badge next to the row: `⟲ shared (idom: ClassName#id)`. Zero extra JSON — the `idom` field is already stored per node for the dominator tree view. This makes "shared ownership" visible without a full retainer list.

   Updated ASCII art with cross-connection badge:
   ```
   ┌──────────────────────────────────────────────────────────────────────────┐
   │  Reference Graph Explorer                       ← Back  Forward →  ⌂    │
   │  HashMap#1234  ›  .table Entry[]#5678  ›  [0] Entry#9012                 │
   ├──────────────────────────────┬───────────────────────────────────────────┤
   │  OUTBOUND REFERENCES         │  DETAILS                                  │
   │  of: Entry#9012              │  java.util.HashMap$Entry                  │
   │  shallow 72B / retained 18MB │  index:    #9012                          │
   │                              │  shallow:  72 B                            │
   │  Filter by field [field ▾]   │  retained: 18 MB  (1.4%)                  │
   │  ┌───┬───────────┬────┬────┬─┤  idom:     Entry[]#5678   [nav →]         │
   │  │ → │ .field    │shl │ret │ │                                            │
   │  ├───┼───────────┼────┼────┼─┤  DOMINATOR CHILDREN                       │
   │  │[→]│.key  Str  │ 24B│ 24B│ │  [into▶] byte[] ×2  (48 B)               │
   │  │[→]│.val  Str  │ 24B│18MB│ │                                            │
   │  │[→]│.next Entry│ 72B│  2B│⟲ shared (idom: Entry[]#5678)               │
   │  └───┴───────────┴────┴────┴─┤  [→ Open in Dominator Explorer]           │
   │  ⟲ = shared object (retained │                                            │
   │      by a different parent)  │                                            │
   └──────────────────────────────┴───────────────────────────────────────────┘
   ```

   The `.next Entry` row shows `⟲ shared (idom: Entry[]#5678)` because Entry's immediate dominator is `Entry[]#5678`, not the current `Entry#9012`. The chain `.next → .next → …` forms a linked list where all nodes are dominated by the array, not by each other — exactly the cross-connection that would be invisible without this badge.

8. **Dynamic depth — no artificial limit**: navigation depth is limited only by the capture set. When a node IS in the capture set (`edges_unknown: false`), clicking `[→]` always works — there is no "maximum depth" cutoff. When a node is NOT in the capture set (`edges_unknown: true`), show: "⚠ This object was not in the top-10,000 captured. [Open OQL query ▶]" — a pre-populated OQL link: `SELECT r FROM <class> r WHERE r = @<id>` opens the OQL tab. Users can always go deeper via OQL for uncaptured nodes.

### Flag: `--obj-graph` (opt-in, off by default)

### Libraries: none beyond existing React + CSS

---

## 4. V4 — Dominator Tree Explorer (retained-heap walk)

### Value

**★★★ The other half of the exploration experience.**

Reference graphs answer "what does this point at?" Dominator trees answer "what does this exclusively retain?" Both are necessary. MAT's "Dominator Tree" is the second most-used view. The unique value over the existing `DomSubtreeSvg`: unlimited depth — the SVG caps at 25 nodes; the table navigator has no depth cap.

**Critical semantic clarification**: the table shows *immediate* dominator children only — one level at a time. `String ×43,200` and `byte[] ×43,200` appear as children of `Entry[]`, not of `HashMap`, because `Entry[]` is their immediate dominator. Users must click `[into▶] Entry[]` to see its children. This is correct dominator semantics and matches MAT's behaviour — but it must be stated in the UI to avoid confusion ("why don't I see String under HashMap?").

Critique: Largely free since it shares the same `dom_children` map as V3. The SVG toggle (existing `DomSubtreeSvg`) requires a `DomTreeNode` tree, which means building one on-demand in the browser from the flat `dom_children` map — or pre-building it in Rust at report time for the top-N root objects. Pre-building for the top-50 roots at depth-3 is cheap and enables the SVG without browser-side tree construction.

### Implementation cost: S (4–6 hours, reuses V3 infrastructure)

The `dom_children` map is stored alongside `edges` in `ObjGraphFlat`. URL hash navigation shared with V3 (`#domtree/NODE_ID`). The table rendering is straightforward. For the SVG mode: pre-build `DomTreeNode` for top-50 roots at depth-3 in `build_obj_graph_flat()` — reuse `build_dominator_tree_node()` already in `report/build.rs`.

### Generation cost: low

Scanning `dc_offsets`/`dc_targets` per BFS-visited node. Already done in the leak suspects path. Pre-building depth-3 SVG trees for top-50 roots: ~3 ms.

### Report size: included in V3 budget

`dom_children` adds ~30% to V3 JSON. Pre-built SVG trees for top-50 roots: ~125 KB extra.

### RSS impact: negligible

### ASCII art

```
┌──────────────────────────────────────────────────────────────────────────┐
│  Dominator Tree Explorer                        ← Back  Forward →  ⌂    │
│  Showing retained subtree of: java.util.HashMap#1234                     │
│  retained: 1.24 GB (93.1% of heap)  ·  floor: ≥ 1 MB                    │
├──────────────────────────────────────────────────────────────────────────┤
│  Filter…                                       📋 Table ✓   🌲 SVG       │
├──────────┬─────────────────────────────┬────────┬──────────┬─────────────┤
│  [nav]   │  Immediate children (×count)│Shallow │ Retained │  % heap     │
├──────────┼─────────────────────────────┼────────┼──────────┼─────────────┤
│  [into▶] │  Entry[]  ×1                │ 16 MB  │  1.18 GB │  91.3%      │
│    —     │  (9 classes, < 1 MB each)   │   —    │    —     │    —        │
└──────────┴─────────────────────────────┴────────┴──────────┴─────────────┘

  ℹ Showing IMMEDIATE dominator children only.
    Entry[] retains String ×43,200 and byte[] ×43,200 — click [into▶] to see them.
  [→ Open in Reference Graph Explorer]
```

**SVG mode** (toggle):

```
┌──────────────────────────────────────────────────────────────────────────┐
│  📋 Table   🌲 SVG ✓                                                     │
│                                                                          │
│  ┌──────────────────────────────────────────────────────────────────┐   │
│  │              HashMap#1234  1.24 GB                               │   │
│  │                    │                                             │   │
│  │             Entry[]  1.18 GB                                     │   │
│  │         ╱─────────────────────╲                                  │   │
│  │   String ×43200          byte[] ×43200                           │   │
│  │    690 MB                 480 MB                                 │   │
│  └──────────────────────────────────────────────────────────────────┘   │
│  (depth 3 pre-rendered · click node to navigate)                        │
└──────────────────────────────────────────────────────────────────────────┘
```

**Key UX details:**

1. **Same URL hash scheme** as V3: `#domtree/1234`. Both explorers coexist in the same report section. A tab bar at the top switches between them: `[📤 Outbound Refs] [🌳 Dominator Tree]`. Active tab reflects URL hash prefix.

2. **Cross-link is bidirectional and lossless**: switching from `#domtree/1234` to `#explore/1234` keeps the same object. The tab bar always shows both options.

3. **"Immediate children only" disclaimer** in the table header: prevents the most common confusion. Users who expect to see the full retained subtree flattened need the V17 sunburst or the existing `DomSubtreeSvg` in the suspects panel.

4. **SVG mode pre-built for roots only**: depth-3 trees for the top-50 root objects are pre-built in Rust. For non-root objects navigated to via `[into▶]`, SVG mode is disabled ("SVG available for top-level objects only — use Table for drill-down"). This avoids building SVG trees for thousands of arbitrary objects in the browser.

5. **Aggregate display for large fan-out**: when a node has 1000+ immediate children all of the same class (e.g. an Object[] dominating 50,000 String instances), aggregate them: `String ×50,000  690 MB  [into▶ one instance]`. Don't list each individual String.

### Flag: `--obj-graph` (same as V3)

### Libraries: existing DomSubtreeSvg (d3-dag) for pre-built SVG; React table for navigation

---

## Heap Explorer: Shared Data Model (V3 + V4)

V3 (Reference Graph Explorer) and V4 (Dominator Tree Explorer) share a single JSON structure. Understanding this model is prerequisite to implementing either.

### Rust structs — `src/report/model.rs`

```rust
/// Flat lookup table powering V3 + V4 navigation.
/// Keyed by dense node index (u32, 1-based in the UI as obj_index_1based).
pub struct ObjGraphFlat {
    /// All significant nodes (BFS-reachable from dominator roots, above sig_floor_bytes).
    pub nodes: HashMap<u32, ObjGraphFlatNode>,
    /// Outbound edges for nodes in the capture set (top-10,000 by shallow heap, captured
    /// before the forward-edge CSR is consumed at analysis time).
    pub edges: HashMap<u32, Vec<ObjGraphEdge>>,
    /// Immediate dominator children for all significant nodes.
    /// dom_children[id] = list of direct children in the dominator tree.
    pub dom_children: HashMap<u32, Vec<u32>>,
    /// Pre-built depth-3 dominator trees for the top-50 root objects.
    /// Enables SVG mode in V4 without browser-side tree construction.
    pub root_dom_trees: Vec<(u32, DomTreeNode)>,
    /// Dense indices of dominator roots (g.idom[i] == VROOT).
    pub roots: Vec<u32>,
    /// Objects with retained < sig_floor_bytes are leaves (no [→] button).
    pub sig_floor_bytes: u64,
}

pub struct ObjGraphFlatNode {
    pub display_class: String,
    pub shallow: u64,
    pub retained: u64,
    /// True when this node was not in the shallow-heap capture set.
    /// V3 shows ⚠ and OQL link; V4 still shows dom_children.
    pub edges_unknown: bool,
    /// True when the edges Vec was capped at edge_cap (100 default).
    pub edges_truncated: bool,
    /// Immediate dominator dense index. None = this node IS a root (idom == VROOT).
    /// Used client-side for cross-connection detection:
    ///   if child.idom != current_node_id → child is shared → show ⟲ badge.
    pub idom: Option<u32>,
}

pub struct ObjGraphEdge {
    pub field_name: String,   // "" when --ref-paths not used
    pub child_idx: u32,
    /// Denormalised for rendering without a second lookup.
    pub child_class: String,
    pub child_retained: u64,
}
```

### Cross-connection detection — zero extra JSON

Every `ObjGraphFlatNode` carries `idom: Option<u32>`. The cross-connection rule:

```
if child.idom.is_some() && child.idom != Some(current_node_id):
    → child is a shared object (retained by a different primary parent)
    → render ⟲ shared (idom: child_class#child_idom_id)
```

This works because:
- The dominator tree is a **tree** — every node has exactly one immediate dominator.
- If you navigate to an object A and see edge A → B, but `B.idom = C` (not A), then B's retention is primarily attributed to C, not A. B is reachable from A but not *exclusively* retained by A.
- No extra JSON needed — `idom` is already stored for the dominator tree view (V4).

### BFS seeding — two distinct sets

1. **`dom_children` / `nodes` BFS** starts from `g.idom[i] == VROOT` (dominator roots). All significant nodes (retained ≥ `sig_floor_bytes`) reachable downward are included. This set is larger than the capture set.

2. **`edges` capture** comes from `ObjGraphCapture` — the top-10,000 objects by *shallow* heap, captured before the forward-edge CSR is consumed. A node can appear in `dom_children` without having an `edges` entry (it gets `edges_unknown: true`).

Significance filter: `sig_floor = max(total_shallow / 1000, 1_048_576)`. Root objects shown only if retained ≥ `root_floor = max(total_shallow / 100, 10_000_000)`.

### Build process — `src/report/build.rs`

```
1. capture_obj_graph_edges(g, top_n=10000, edge_cap=100)    ← before inbound CSR consumed
2. compute_retained(g)                                      ← dominator pass
3. build_obj_graph_flat(g):
   a. compute sig_floor, root_floor
   b. BFS from VROOT children → populate nodes{} + dom_children{}
   c. For each node in capture: copy edges → edges{}
   d. Pre-build root_dom_trees for top-20 roots at depth 3
      (reuse build_dominator_tree_node() from leak-suspects path)
```

Step 1 must happen before step 2 (the CSR is consumed during inbound construction). This is the hard constraint that forces the two-pass capture design.

### Size budget

| Component | Typical (1 GB heap) | Large (4 GB heap) |
|-----------|--------------------|--------------------|
| nodes{} | ~80 KB | ~320 KB |
| edges{} | ~300 KB | ~1.2 MB |
| dom_children{} | ~30 KB | ~120 KB |
| root_dom_trees | ~50 KB | ~50 KB (fixed, top-20 only) |
| **Total raw** | **~460 KB** | **~1.7 MB** |
| **Compressed** | **~50 KB** | **~180 KB** |

---

## 5. V5 — "Who Holds This Class?" Immediate-Dominator Lookup

### Value

**★★★ Answers the #1 StackOverflow heap dump question (2,847 votes) inline.**

"I see 4 million byte[] instances — who is holding them all?" Without this, users must run OQL, navigate the dominator tree, or use MAT's "Merge Shortest Paths" query. JProfiler's "Merged Dominating References" is their most praised feature for this exact workflow.

The immediate-dominator distribution for a class is the single most actionable diagnostic: "87% of byte[] is dominated by String, 9% by char[], 3% by DirectByteBuffer" tells you exactly where to look.

**Sankey toggle**: the histogram expansion is useful as a table, but a sankey makes the *flow of retained heap* immediately legible. Left column = dominator classes, right column = target class (e.g. `byte[]`), link widths ∝ retained bytes attributed. At a glance: one thick band means one class owns almost everything; many thin bands means scattered ownership.

**Click-through drill-down**: clicking a dominator node in the sankey (e.g. `DirectByteBuffer`) pivots the view — it becomes the new target class on the right, and *its* dominator breakdown appears on the left. This allows walking back up the ownership chain interactively: `byte[]` → click `DirectByteBuffer` → see who holds `DirectByteBuffer` → click `Cleaner` → see who holds `Cleaner`, etc. A breadcrumb trail tracks the path. The data for each step is already in the report (the `ImmediateDominators` table covers all histogram classes); no new requests are needed — the browser pivots over the same JSON dataset.

Critique: The backend data model (`ImmDomPair` struct + `ImmediateDominators.pairs: Vec<ImmDomPair>`) is **already implemented** in `src/report/model.rs` and `src/report/build.rs`. The build pass emits `(dominator_class, dominated_class, pair_count, dominated_shallow, dominated_retained)` triples, capped at 20,000 pairs sorted by `dominated_retained`. The frontend sankey is the remaining work.

### Implementation cost: S–M (frontend only — backend already done)

**Backend** (done): `ImmDomPair` struct in `report/model.rs`; pair aggregation in `build_dominator_analysis()` in `report/build.rs` (constant `IMDOM_PAIRS_CAP = 20_000`).

**Frontend**: table expansion (existing design) + sankey toggle using d3-sankey. The sankey is **two-sided**: left = dominator classes (who holds the target), right = dominated classes (what the target holds). Both sides read from `dominator_analysis.immediate_dominators.pairs` — left queries rows where `dominated_class == target`, right queries rows where `dominator_class == target`. Clicking any node on either side pivots it to centre. Cap each side to top-8 links by `dominated_retained` to stay readable. Breadcrumb tracks pivot history.

### Generation cost: low

O(n_instances_of_class) × number of histogram classes using existing `idom[]` array. A few hundred ms worst-case for classes with millions of instances.

### Report size: ~130 KB uncompressed / ~15 KB compressed

Top-20,000 `ImmDomPair` records × ~80 bytes each → ~1.6 MB uncompressed, ~80 KB compressed. Dense coverage: for a heap with 2,000 significant classes this gives ~10 dominator pairs per class — enough to drive the full two-sided sankey navigation. The cap is set high because this data is the backbone of the ownership walk-through; sparse coverage makes the interactive drill-down hit dead ends too quickly.

### RSS impact: negligible

Pair aggregation uses a `HashMap<(u32,u32), (u64,u64,u64)>` during the build pass. For 20,000 class pairs × ~60 bytes: ~1.2 MB. Freed immediately after.

### ASCII art

Table view (default, collapsed):
```
CLASS HISTOGRAM
┌──────────────────────────────────┬─────────┬──────────┬──────────┐
│  Class                           │Instances│  Shallow │ Retained │
├──────────────────────────────────┼─────────┼──────────┼──────────┤
│ ▶ byte[]                    [+]  │ 4.2 M   │  840 MB  │  840 MB  │
│   └─ Who holds these? [Table ✓] [Sankey]  │          │          │
│      String          ×3.8M  89%  │         │   749 MB │          │
│      DirectByteBuffer   ×120  3%  │         │    25 MB │          │
│      (other classes)     8%      │         │          │          │
│                                  │         │          │          │
│ ▶ java.util.HashMap         [+]  │  12,400 │  298 MB  │  1.24 GB │
│ ▶ java.lang.String          [+]  │ 3.8 M   │  690 MB  │  690 MB  │
└──────────────────────────────────┴─────────┴──────────┴──────────┘
```

Sankey view (toggle) — initial view for `byte[]`:
```
  WHO HOLDS byte[]?   [Table] [Sankey ✓]
  byte[]  (breadcrumb: byte[])

  ┌──────────────┐                    ┌──────────┐
  │ String       │════════════════════│          │
  │  ×3.8M, 89% │                    │  byte[]  │
  ├──────────────┤                    │  4.2M    │
  │ DirectBB     │══╗                 │  840 MB  │
  │  ×120,  3%  │  ╚══════════════════│          │
  ├──────────────┤                    └──────────┘
  │ char[]       │═╗
  │  ×800,  5%  │ ╚═══════════════════(thin band)
  └──────────────┘
  (link width ∝ retained bytes · click any left node to drill into it)
```

After clicking `DirectByteBuffer` on the left — it pivots to become the target. Full size info on every node; right side expands to show what DirectByteBuffer itself dominates:
```
  WHO HOLDS DirectByteBuffer?   [Table] [Sankey ✓]
  byte[] › DirectByteBuffer  (click breadcrumb to go back)

  ┌─────────────────────┐                      ┌──────────────────────┐
  │ sun.nio.ch          │══════════════════════▶│                      │══╗ ┌──────────┐
  │ FileChannelImpl     │  ×80 · 17 MB retained │  DirectByteBuffer    │  ╚▶│ Cleaner  │
  │  ×80 · 67%         │                       │  ×120 · 25 MB        │    │ ×120     │
  ├─────────────────────┤                       │  shallow: 3.8 KB     │    │  4 MB    │
  │ java.nio            │══╗                    │  retained: 25 MB     │    └──────────┘
  │ MappedByteBuffer    │  ╚════════════════════▶│                      │══╗ ┌──────────┐
  │  ×40 · 33% · 8 MB  │                       └──────────────────────┘  ╚▶│ byte[]   │
  └─────────────────────┘                                                    │  ×120    │
                                                                             │  21 MB   │
                                                                             └──────────┘
  Left = who retains DirectByteBuffer  ·  Right = what DirectByteBuffer dominates
  (click any node on either side to pivot to it)
```

**Data sources for both sides**:
- Left (who holds): `ImmediateDominators` rows where `dominated_class == "DirectByteBuffer"` — gives `(dominator_class, count, retained)`.
- Right (what it dominates): `ImmediateDominators` rows where `dominator_class == "DirectByteBuffer"` — gives `(dominated_class, count, retained)`. Same dataset, different query direction. No new backend data needed.

This makes the sankey a **two-sided navigator**: left shows inbound ownership, right shows outbound domination. Clicking any node on either side pivots it to centre. This is the full "Merged Dominating References" experience — walk the ownership graph in both directions from any class.

### Flag: always-on

### Libraries: d3-sankey (to add); backend already implemented

---

## 6. V6 — DirectByteBuffer Off-Heap Card

### Value

**★★★ Prevents the most expensive invisible leak.**

Production case study #6: 40 GB of native memory committed by NIO buffers. The JVM heap showed only 200 MB. Without this card, users dismiss the heap dump as "fine" and spend days debugging native memory. The `direct_byte_buffer_capacity_sum` field already exists in `LeakIndicators` but is buried.

Critique: Trivial to implement. The only question is whether to also show a classloader breakdown (which component is creating the buffers). That would require a small backend addition; the basic card is sufficient initially.

### Implementation cost: S (1 hour, frontend only)

Promote `leak_indicators.direct_byte_buffer_capacity_sum` to a prominent card. Add triage badge if > 256 MB.

### Generation cost: none

### Report size: none

### RSS impact: none

### ASCII art

```
┌──────────────────────────────────────────────────────────┐
│  ⚠ OFF-HEAP NIO MEMORY                                   │
│                                                          │
│  1.24 GB committed across 3,400 DirectByteBuffer objects │
│  (not visible in the JVM heap — native memory)           │
│                                                          │
│  This memory is NOT counted in the heap totals above.    │
│  Check for NIO buffer pools that are not being released. │
└──────────────────────────────────────────────────────────┘
```

### Flag: always-on

### Libraries: none

---

## 7. V7 — Classloader Heatmap (ZoomableTreemap)

### Value

**★★★ Makes classloader leaks visible in 5 seconds.**

HeapHero's "Histogram by ClassLoader" view. Eclipse MAT's `class_loader_explorer`. The community pain point: "Classloader leaks are invisible in MAT unless you know exactly where to look." A ZoomableTreemap of `loader_rollup` makes it obvious: when you see 48 equally-sized squares labelled "URLClassLoader", that's a Metaspace leak.

Critique: The `loader_rollup` data is already present. Wiring it into `ZoomableTreemap` is trivial. The only design question is click behaviour — clicking a loader should show the classes it loaded. This requires the treemap to support a "second level" drill-down showing `loader_rollup[i].classes`. The existing ZoomableTreemap already supports this drill-down pattern (it's how the Biggest Packages view works).

### Implementation cost: S (2–3 hours, frontend only)

Wire `loader_rollup` into `ZoomableTreemap`. Add click handler for class drill-down.

### Generation cost: none

### Report size: none

### RSS impact: none

### ASCII art

```
CLASSLOADER RETAINED HEAP
┌──────────────────────────────────────────────────────────┐
│                                                          │
│  BootstrapClassLoader      URLClassLoader ×48            │
│  ┌──────────────────────┐  ┌────────────────────────┐   │
│  │                      │  │ UCL  UCL  UCL  UCL  UCL │   │
│  │  1.2 GB  (all JDK    │  │ UCL  UCL  UCL  UCL  UCL │   │
│  │  classes, normal)    │  │ UCL  UCL  UCL  UCL  UCL │   │
│  │                      │  │ … 48 instances  320 MB  │   │
│  └──────────────────────┘  └────────────────────────┘   │
│                                                          │
│  AppClassLoader            WebAppClassLoader ×3          │
│  ┌──────────┐              ┌──────────────────────┐      │
│  │  280 MB  │              │ WAR1  WAR2  WAR3      │      │
│  └──────────┘              │        90 MB          │      │
│                            └──────────────────────┘      │
└──────────────────────────────────────────────────────────┘
```

### Flag: always-on

### Libraries: ZoomableTreemap (already in bundle)

---

## 8. V8 — Lambda / Anonymous Class Grouper

### Value

**★★ Turns opaque lambda spam into actionable signal.**

Reddit r/java: "Lambda names in MAT are completely opaque — I see `$$Lambda$4821/0x00007f2b3c` and have no idea what it is." Production case study #9: 2.1 GB lambda closure leak hidden behind 400 distinct `$$Lambda$NNN` class names, each with ~5 MB. Individually each name looks fine; grouped by enclosing class the leak is obvious.

Critique: Pure client-side transformation — no backend. The naive "strip from first `$`" regex covers the common JDK lambda pattern (`$$Lambda$NNN`) and most inner classes, but has false-positive risk on:
- Scala classes that use `$` as a name character (e.g. `scala.collection.immutable.$colon$colon` — grouping all `$colon$colon` variants under `scala.collection.immutable` is correct, but Scala operators like `$plus$plus` look opaque grouped)
- Classes intentionally named with `$` as a separator in non-lambda contexts

Better heuristic: only group when the suffix after `$` is either (a) `Lambda$` followed by digits, or (b) a pure-digit suffix. This covers 98% of the target cases while avoiding false grouping of Scala operator classes. The toggle lets users switch to raw names when the grouper misfires.

### Implementation cost: S (2–3 hours, frontend only)

Client-side transformation of `histogram` rows. Regex strip + group by prefix. Toggle switch in histogram header.

### Generation cost: none

### Report size: none

### RSS impact: none

### ASCII art

```
CLASS HISTOGRAM  [Group lambdas ✓] [Show all classes]

┌──────────────────────────────────────────────────┬──────────┬──────────┐
│  Class                                           │Instances │ Shallow  │
├──────────────────────────────────────────────────┼──────────┼──────────┤
│ ▶ java.util.stream.ReferencePipeline [λ ×3,421]  │  89,000  │   45 MB  │
│   ├─ $$Lambda$421/0x00007f...  ×12,000  16 MB    │          │          │
│   ├─ $$Lambda$422/0x00007f...  × 9,400  12 MB    │          │          │
│   └─ (3,419 more lambda variants)                │          │          │
│                                                  │          │          │
│ ▶ com.myapp.EventBus [λ ×1,200, $Inner ×80]      │  24,000  │   12 MB  │
└──────────────────────────────────────────────────┴──────────┴──────────┘
```

### Flag: always-on (toggle within UI)

### Libraries: none

---

## 9. V9 — Collection Waste Budget Table

### Value

**★★★ Turns JOverflow's 16 waste categories into one actionable table.**

HeapHero's "Waste Advisor". The community pain point: "I know there's waste in my collections — I just can't find it quickly." This table does the aggregation that users currently do manually by cross-referencing four different sections of the report.

Critique: All data present under `--collections`. The aggregation is client-side. The only design risk is misleading "fix suggestions" — keep them generic ("consider trimming capacity", "consider using primitive arrays") to avoid bad advice on valid use cases.

### Implementation cost: S (2–3 hours, frontend only)

Client-side aggregation of `collection_attribution`, `duplicate_strings`, `arrays_by_size`. Sorted by wasted bytes.

### Generation cost: none (requires `--collections` flag for the underlying data)

### Report size: none (reads existing fields)

### RSS impact: none

### ASCII art

```
COLLECTION WASTE BUDGET  (requires --collections)
┌──────────────────────────────────────────────┬──────────┬──────────┬──────────────────────┐
│  Waste type / location                       │  Wasted  │ Objects  │  Fix                 │
├──────────────────────────────────────────────┼──────────┼──────────┼──────────────────────┤
│  Under-filled: HashMap (load < 25%)          │  340 MB  │  12,400  │  Trim or resize      │
│  Empty ArrayList instances                   │  120 MB  │  48,000  │  Use Collections.EMPTY│
│  Duplicate strings (same content)            │   88 MB  │ 890,000  │  String.intern()     │
│  Oversized backing arrays (HashMap)          │   45 MB  │   3,200  │  Use initialCapacity │
│  Boxed primitives (Integer in ArrayList)     │   12 MB  │ 120,000  │  Use IntArrayList    │
│  Constant arrays (all-zero byte[])           │    8 MB  │   4,100  │  Deduplicate         │
└──────────────────────────────────────────────┴──────────┴──────────┴──────────────────────┘
  Total identified waste: 613 MB  (15.3% of reachable heap)
```

### Flag: only meaningful when `--collections` data is present; table shown when data exists

### Libraries: react-data-table-component (already bundled)

---

## 10. V10 — Allocation Site Flamegraph

### Value

**★★ Identifies leaks by code location when allocation traces are present.**

async-profiler's allocation flamegraph mode. JProfiler's "Call Tree sorted by alive allocations". When HPROF contains allocation traces (rare in production, common in dev/staging), a flamegraph sorted by **retained** bytes shows exactly which call path's objects are still alive. This is categorically different from profilers that show allocated bytes — it shows which allocations **failed to be collected**.

Critique: **Allocation traces are present in fewer than 5% of production heap dumps.** Most production JVMs run without `-agentlib:hprof=heap=all` because it imposes significant overhead. This view is high-value when traces are present, near-zero-value when absent. It should be hidden completely when `alloc_sites.traces_present = false` — do not show a placeholder section. Medium implementation effort for limited reach, but the implementation reuses `ZoomableTreemap` directly so the marginal cost is low. Worth building; not worth advertising prominently.

### Implementation cost: M (3–5 hours frontend)

Wire `alloc_sites` frames into `ZoomableTreemap` with flamegraph mode. The existing component already does this for packages; the data model is analogous.

### Generation cost: none

### Report size: none (reads existing `alloc_sites` field)

### RSS impact: none

### ASCII art

```
ALLOCATION SITE FLAMEGRAPH  (sorted by retained bytes)
Only shown when HPROF contains allocation traces.

retained →
┌──────────────────────────────────────────────────────────────────────┐
│                    all frames  (890 MB retained)                     │
├──────────────────────────────────────────┬───────────────────────────┤
│   com.myapp.RequestHandler.handle()      │   com.myapp.CacheManager  │
│           (640 MB)                       │       (250 MB)            │
├────────────────────┬─────────────────────┼───────────────────────────┤
│ fetchFromDB()      │ processResponse()   │ CacheManager.put()        │
│    (580 MB)        │     (60 MB)         │     (250 MB)              │
├────────────────────┤                     ├───────────────────────────┤
│ JDBC.query()       │                     │ HashMap.put()             │
│    (580 MB)        │                     │     (250 MB)              │
└────────────────────┴─────────────────────┴───────────────────────────┘
  Click a frame to zoom in. Widths proportional to retained bytes.
```

### Flag: always-on but only rendered when `alloc_sites.traces_present = true`

### Libraries: ZoomableTreemap (already in bundle)

---

## 11. V12 — Cross-Dump Retained Growth Diff

### Value

**★★★ The gold-standard leak detection workflow: two dumps, 5 minutes apart.**

dotMemory "Traffic View". Go pprof `inuse_space` vs `alloc_space`. Production workflow: take dump A (baseline), wait 5 minutes, take dump B. Classes that grew retained heap between A and B are the leak. Classes where retained heap is stable are not leaking even if they're large.

Currently the diff report shows shallow-heap deltas only. Retained deltas are categorically more useful for leak detection.

Critique: Requires a small model change (`add retained to diff HistRow`) and one extra field populated in `diff_reports.rs`. The UI is straightforward — add a `Δ retained` column to the existing diff table. Medium effort overall.

### Implementation cost: M (2–3 hours backend + 1 hour frontend)

Add `retained_a`, `retained_b`, `retained_delta` to diff row in `report/model.rs`. Populate in `diff_reports.rs` by matching class names. New table column in `DiffApp`.

### Generation cost: low

Already computing retained heap per class for each dump. Just needs serialisation.

### Report size: ~30 KB extra in diff report

### RSS impact: none

### ASCII art

```
RETAINED HEAP GROWTH  (Dump A → Dump B, 5 min apart)
┌────────────────────────────────┬──────────┬──────────┬──────────┬──────────┐
│  Class                         │Retained A│Retained B│  Δ bytes │    Δ %   │
├────────────────────────────────┼──────────┼──────────┼──────────┼──────────┤
│  ▲ java.util.HashMap           │  820 MB  │  1.24 GB │ +430 MB  │  +52.4%  │
│  ▲ byte[]                      │  480 MB  │  640 MB  │ +160 MB  │  +33.3%  │
│  ▲ com.myapp.CacheEntry        │   80 MB  │  210 MB  │ +130 MB  │ +162.5%  │
│  ≈ java.lang.String            │  420 MB  │  418 MB  │   -2 MB  │   -0.5%  │
│  ▼ com.myapp.Request           │   90 MB  │   12 MB  │  -78 MB  │  -86.7%  │
└────────────────────────────────┴──────────┴──────────┴──────────┴──────────┘
  Growing classes ▲ are leak candidates. Stable ≈ or shrinking ▼ are not.
```

### Flag: diff mode only (automatically present in diff reports)

### Libraries: react-data-table-component (already bundled)

---

## 12. V13 — Type-Level Reference Graph (TPFG)

### Value

**★★★ The heap's structure at a glance — three complementary views, not available in any commercial tool today.**

Cork POPL 2007 (Type Points-From Graph). HeapViz ISMM 2011 merge rules. Instead of individual objects, shows the heap at the class level: "HashMap → Entry[] (×4.2M edges, 1.1 GB retained weight)". Answers "what is the reference topology of this heap?" before the user has read a single histogram row.

**Why a sankey is the right visualisation here**: the TPFG is fundamentally a *flow* problem — retained heap originates in GC roots, flows through reference edges, and accumulates in leaf classes. A sankey with nodes = classes and link widths ∝ retained weight shows this flow directly. The layout is left-to-right by dominator depth (most-dominating classes on the left, leaf classes on the right). A force-directed graph with 50 class nodes requires careful pre-computed layout to be readable; a sankey of the top-15 retained-weight edges is immediately legible to anyone.

**Three views, one dataset**:
- **Sankey** (default): top-15 edges by retained weight, left-to-right by dominator depth, node height ∝ total retained. Best for "what's the dominant flow?" — legible immediately with no interaction required.
- **Force graph** (toggle): d3-force simulation in the browser. All class nodes + edges. Drag nodes to reposition, zoom, click a node to highlight its incoming/outgoing edges, double-click to pin. Enables exploration of the full topology beyond the top-15 edges. d3-force runs live in the browser — 5.5 KB, no Rust pre-computation, fully interactive.
- **Adjacency table** (toggle): all class pairs sorted by edge count or retained weight. Best for "find the edge between class X and class Y."

Critique: Medium backend effort (new aggregation pass). The sankey degrades to visual noise when the heap has many medium-weight edges of similar size (e.g. a microservices heap with 200 classes all roughly equal). Mitigate by capping at top-15 edges and providing the force graph and table as fallbacks. The force graph handles the high-edge-count case well — nodes cluster naturally by retention weight, and dragging reveals structure.

d3-sankey computes its own left-to-right layout; d3-force runs client-side simulation. No Rust pre-computation of positions needed for either.

### Implementation cost: M (1 day backend + half day frontend)

**Backend**: new pass in `report/build.rs`. Scan `fwd_targets` once (O(all_edges)), aggregate into `HashMap<(src_class_idx, dst_class_idx), (edge_count, retained_weight)>`. Filter to top-N pairs by retained weight. Emit as `type_ref_graph: Vec<TypeEdge>`. No position coordinates — layout is done in the browser by d3-sankey and d3-force respectively.

```rust
pub struct TypeEdge {
    pub src_class: String,
    pub dst_class: String,
    pub edge_count: u64,
    pub retained_weight: u64,  // sum of src-object retained × (1/out-degree) heuristic
}
```

**Frontend**: `TypeRefGraph` component with three toggle tabs: Sankey / Force / Table.
- Sankey: d3-sankey, nodes from unique class names, links = TypeEdge entries, node color by layer depth.
- Force: d3-force simulation, `forceLink` + `forceManyBody` + `forceCenter`. Node radius ∝ retained. Click to highlight connected edges. Drag to pin. Zoom via SVG viewBox transform.
- Table: StdTable with src/dst/edge_count/retained_weight columns, sortable.

### Generation cost: medium

O(all_edges) scan: ~200–500 ms for 4 GB heaps. No Rust layout computation. Aggregation is a single HashMap pass.

### Report size: ~50 KB

Top-500 `TypeEdge` records × ~100 bytes each = ~50 KB uncompressed. Compressed: ~5 KB. Dense enough to power both the sankey (top-15 edges rendered, rest available for the force graph and table) and the full force graph with all class nodes.

### RSS impact: ~1 MB peak during aggregation

HashMap of `(u32, u32) → (u64, u64)` for class-pair aggregation. For 100k class pairs × 24 bytes: ~2.4 MB peak. Freed immediately after aggregation.

### ASCII art

Sankey view (default):
```
TYPE REFERENCE GRAPH   [Sankey ✓] [Force] [Table]
  (node height ∝ retained heap  ·  link width ∝ retained weight flow)

  ┌──────────────┐
  │   HashMap    │══════════════════════════════╗  ┌──────────┐
  │    1.24 GB   │  .table  ×4.2M  1.1 GB       ╚══│  Entry[] │══╗  ┌────────┐
  └──────────────┘                                  │  1.18 GB │  ╚══│ String │
  ┌──────────────┐                                  │          │  ╔══│ 690 MB │
  │  ArrayList   │══════════════════════════════════│          │══╝  └────────┘
  │   320 MB     │                              ════└──────────┘     ┌────────┐
  └──────────────┘                                               ╔══│ byte[] │
  ┌──────────────┐                                               ║  │ 480 MB │
  │   String     │═══════════════════════════════════════════════╝  └────────┘
  │   690 MB     │
  └──────────────┘
```

Force graph view (toggle) — interactive, d3-force in browser:
```
TYPE REFERENCE GRAPH   [Sankey] [Force ✓] [Table]
  drag nodes · scroll to zoom · click to highlight · dbl-click to pin

          ╭──────────╮
          │ HashMap  │──────────────────╮
          ╰──────────╯  .table ×4.2M   │
                │                      ▼
          .entrySet               ╭──────────╮      .key    ╭────────╮
                │                 │  Entry[] │──────────────│ String │
                ▼                 ╰──────────╯              ╰────────╯
          ╭─────────╮                  │                       │
          │   Set   │             [i]  │ .value           .value│
          ╰─────────╯                  ▼                       ▼
                                 ╭─────────╮             ╭────────╮
                                 │  Entry  │─────────────│ byte[] │
                                 ╰─────────╯             ╰────────╯

  (all class pairs shown · node radius ∝ retained · edge weight ∝ retained weight)
  [highlighted node: Entry[] — incoming: HashMap .table; outgoing: String .key, byte[] .value]
```

Adjacency table view (toggle):
```
TYPE REFERENCE GRAPH   [Sankey] [Force] [Table ✓]
┌────────────────┬──────────────────┬───────────┬──────────────┐
│  From class    │  To class        │ Edge count│ Retained wt  │
├────────────────┼──────────────────┼───────────┼──────────────┤
│ HashMap        │ Entry[]          │   4.2 M   │    1.1 GB    │
│ Entry[]        │ String (key)     │   4.2 M   │    690 MB    │
│ Entry[]        │ byte[] (val)     │   4.2 M   │    480 MB    │
│ ArrayList      │ Object[]         │   1.2 M   │    310 MB    │
│ …              │ …                │    …      │     …        │
└────────────────┴──────────────────┴───────────┴──────────────┘
```

### Flag: `--obj-graph` (same flag as V3/V4; all exploration features together)

### Libraries: d3-sankey (sankey view) + d3-force (force-graph view) — both to add, ~13.4 KB total

---

## 13. V15 — GC Root Type × Class Table

### Value

**★★ Identifies whether a leak originates from JNI, threads, or the JVM.**

Eclipse MAT `gc_roots` inspector. Production case study #4: JNI GlobalRef leak at 8 GB — invisible without this table because the objects looked unremarkable in the histogram. The key diagnostic: "84% of retained heap is held by JNI Global Roots" vs "normal: nearly all retained heap held by Thread roots."

Critique: Currently `gc_roots_by_type` shows only counts per root type, not which classes are held. A small backend addition gives a per-class breakdown per root type. Low effort, high value for JNI leak diagnosis.

### Implementation cost: S (2–3 hours backend + 1 hour frontend)

Extend `gc_roots_by_type` in `report/build.rs` to include top-5 retained classes per root type.

### Generation cost: low

One scan over GC root list + class lookup per root. O(n_roots) = fast.

### Report size: < 5 KB

### RSS impact: none

### ASCII art

```
GC ROOTS BY TYPE
┌─────────────────────┬──────────┬────────────────────────────────────────────┐
│  Root type          │  Count   │  Top retained classes                      │
├─────────────────────┼──────────┼────────────────────────────────────────────┤
│  Thread             │   1,248  │  HashMap (1.2 GB), byte[] (480 MB), ...    │
│  JNI Global         │  14,208  │  byte[] (8.1 GB !!), NativeBuffer (200 MB) │
│  JNI Local          │     420  │  Object[] (12 MB)                          │
│  System Class       │   4,200  │  Class (42 MB)                             │
│  Monitor Used       │      18  │  Object (1 MB)                             │
└─────────────────────┴──────────┴────────────────────────────────────────────┘
  ⚠ JNI Global holds 8.1 GB — likely a native code reference leak.
```

### Flag: always-on

### Libraries: react-data-table-component (already bundled)

---

## 15. V16 — ThreadLocal Leak Analyzer

### Value

**★★★ Detects the #1 production leak pattern.**

YourKit's "Thread local variables" inspection — their most commonly used inspection by user survey. Production case study #3: ThreadLocal leak at 4.3 GB in a Tomcat application. HN: "ThreadLocals are invisible in every tool I've used except YourKit."

The current implementation shows only a stale entry count. This adds the full class breakdown: which classes are stored in ThreadLocals, how many stale entries exist per class, and total retained heap. "HttpSession ×47 stale entries, 180 MB retained" is immediately actionable. "47 stale ThreadLocal entries" is not.

Critique: Requires a field-decode scan of `ThreadLocal$ThreadLocalMap$Entry` — a Tier-B scan not currently implemented. Medium backend effort. The scan decodes the `referent` (key, may be null = stale) and `value` fields. Class name lookup via the existing field-decode infrastructure.

### Implementation cost: M (1 day backend + 2 hours frontend)

New scan pass in `pass2/` decoding ThreadLocalMap$Entry instances. Add `ThreadLocalLeakRow` to `report/model.rs`. New section in Threads panel.

### Generation cost: medium

One field-decode scan pass over all ThreadLocalMap$Entry instances. For large heaps with many threads: ~50–200 ms extra.

### Report size: < 5 KB

### RSS impact: ~10 MB peak during scan

Holding decoded entry data in memory during the scan pass. Freed after aggregation.

### ASCII art

```
THREADLOCAL LEAK ANALYSIS
┌──────────────────────────────────┬──────────┬──────────┬─────────────────┐
│  Value class                     │ Entries  │  Stale   │  Retained       │
├──────────────────────────────────┼──────────┼──────────┼─────────────────┤
│  com.myapp.HttpSession           │    3,420 │       47 │  180 MB  ⚠      │
│  org.hibernate.Session           │      840 │       12 │   44 MB  ⚠      │
│  java.util.HashMap               │      120 │        0 │    8 MB         │
│  com.myapp.RequestContext         │       88 │        3 │    2 MB         │
└──────────────────────────────────┴──────────┴──────────┴─────────────────┘
  Stale entries = null key (GC collected key, value not freed).
  ⚠ Entries with null keys should be cleaned up via remove().
```

### Flag: `--find-duplicates` or `--full-analysis` (prefer `--full-analysis`)

`--find-duplicates` already gates Tier-B scans and is technically correct, but semantically misleading — ThreadLocal analysis has nothing to do with duplicate objects. `--full-analysis` (if it exists or is added) is the better long-term gate. Short term: accept either flag.

### Libraries: none

---

## 16. V18 — Reference Chain Graph (Root Path Visualisation)

### Value

**★★★ Makes the leak path comprehensible at a glance — and for group suspects, shows which intermediate classes carry the most weight.**

Eclipse MAT "Path to GC Roots" diagram. The current report shows root paths as text (`Thread "http-exec-3" → RequestHandler.session → SessionData.cache → HashMap`). A visual with widths proportional to retained heap is substantially easier to scan when paths are 5–15 steps long — and for group suspects with branching paths, the width variation is the key signal.

**Two different suspect types need two different approaches:**

**Single suspect** (has `root_path: Vec<RootPathStep>`): the path is a linear chain. A vertical SVG chain (`DomSubtreeSvg`-style) with edge labels is ideal. Widths are equal (every hop retains the same ~1.24 GB), so width variation doesn't help — but the layered layout with field names is already more readable than text. The existing `TreeSvg`/`DomSubtreeSvg` infrastructure handles this.

**Group suspect** (has `merged_paths: MergedPathNode`): the path is a *tree* — the dominator chains of all N member objects collapsed into a class-keyed prefix tree. Nodes carry `count` (how many chains pass through) and `retained` (sum of those chains' retained heap). This is genuinely a flow: the root carries the full group retained, branches show which intermediate class carries more. A **horizontal sankey** is the right visualisation: left = GC root, right = accumulation point, link widths ∝ `retained` at each node. Width variation is the signal — a thick band through `HttpSession` and thin bands through everything else tells you exactly where to look.

**Critique**: The existing text representation already conveys the structure for linear paths. The sankey adds genuine value only for `merged_paths` (group suspects), which is where the branching and width variation are meaningful. Single-suspect paths get the SVG chain upgrade; group suspects get the sankey. Both improvements are frontend-only — the data already exists.

### Implementation cost: M (3–5 hours frontend)

**Single suspect path** (upgrade from text to SVG): new `RootPathChain` component in `domTree.tsx`. Vertical layered layout reusing `TreeSvg` idioms. Each node: class name, shallow, field edge label. No width variation needed.

**Group suspect merged path** (new sankey): `MergedPathSankey` component using d3-sankey. Convert `MergedPathNode` tree to d3-sankey `{nodes, links}` format: each distinct class in the tree becomes a node, each parent→child edge becomes a link with `value = child.retained`. Node widths ∝ retained. Clicking a node shows the class name, count, and retained in a tooltip.

### Generation cost: none

Both `root_path` and `merged_paths` are already built in `build_leak_suspects()`.

### Report size: none

No new data. The visualisation is built client-side from existing `root_path` / `merged_paths` JSON.

### RSS impact: none

### ASCII art

Single-suspect SVG chain:
```
ROOT PATH  [Chain ✓]   Thread "http-exec-3" → HashMap#1234

  ╔══════════════════════════════╗
  ║ Thread "http-exec-3"         ║  GC Root
  ╚══════════════════════════════╝
           │ .localVariables
           ▼
  ┌──────────────────────────────┐
  │ RequestHandler#8821          │  400 B shallow
  └──────────────────────────────┘
           │ .session
           ▼
  ┌──────────────────────────────┐
  │ HttpSession#3312             │  1.2 KB shallow
  └──────────────────────────────┘
           │ .attributeMap
           ▼
  ╔══════════════════════════════╗
  ║ HashMap#1234  ← SUSPECT      ║  1.24 GB retained
  ╚══════════════════════════════╝
  (field names shown when --ref-paths used)
```

Group-suspect sankey (merged paths for 12,400 HashMap instances):
```
RETENTION PATHS  [Sankey ✓]  12,400 HashMap instances · 1.24 GB total

GC Root layer               Intermediate layer              Accumulation

╔══════════════╗
║ Thread       ║═════════════════════════╗
║ ×8,200       ║  ×8,200 · 1.02 GB       ╚══╗ ╔════════════════╗
║  1.02 GB     ║                             ╚═║ HttpSession    ║══╗
╚══════════════╝                           ╔══║ ×12,200 · 1.2GB║  ║
╔══════════════╗                           ║  ╚════════════════╝  ║
║ ClassLoader  ║═══════════════════════════╝                      ║  ╔═══════════╗
║ ×4,000       ║  ×4,000 · 180 MB                                 ╚══║  HashMap  ║
║  180 MB      ║                                                      ║  ×12,400  ║
╚══════════════╝                                                      ║  1.24 GB  ║
                                                                      ╚═══════════╝
(link width ∝ retained · hover for count and exact bytes)
```

### Flag: always-on; SVG chain rendered when `root_path` non-empty; sankey rendered when `merged_paths` non-null

### Libraries: d3-sankey (group suspect sankey); existing TreeSvg from domTree.tsx (single suspect chain)

---

## 18. V19 — Framework Auto-Analysis (Spring / Hibernate / Netty / Executors)

### Value

**★★★ Proactive detection — shows results automatically when framework classes are present.**

JProfiler "Framework Inspections" panel, HeapHero automatic framework detection. Production case study #7: Hibernate session not closed = 900 MB retained. Instead of surfacing query *suggestions*, we run the analyses at report-generation time and display inline cards when relevant classes are detected. The user never has to ask.

**Framework detectors** (each runs only when its sentinel class is found in the histogram):

| Framework | Sentinel class | What we compute |
|-----------|---------------|-----------------|
| Hibernate | `org.hibernate.internal.SessionImpl` | Live session count, first-level cache size per session, total retained |
| Spring | `org.springframework.context.support.AbstractApplicationContext` | Context count, context type, retained heap per context |
| ThreadPoolExecutor | `java.util.concurrent.ThreadPoolExecutor` | Pool count, queue depth per pool, queued task retained |
| Netty | `io.netty.buffer.AbstractReferenceCountedByteBuf` | Live buffer count, total capacity, refCnt distribution |
| JDBC connection pools | `com.zaxxer.hikari.pool.HikariPool` or `c3p0.*` | Pool instances, connection count, pool retained |

Each result is displayed as a dedicated card in a "Framework Analysis" section. Hidden entirely when no framework classes are detected.

Critique: Each detector requires a field-decode scan pass — this promotes V19 from S to M effort. However, each scan is conditional on the sentinel class being present, so heaps without that framework pay no cost. The value is substantially higher than query suggestions — users get answers, not prompts.

### Implementation cost: M (1–2 days backend: one scan pass per detected framework)

New framework scan infrastructure in `pass2/framework_scan.rs`. Each detector is a separate function; the dispatch checks histogram class names before running. Add `FrameworkAnalysis` to `report/model.rs`.

### Generation cost: medium (conditional: only runs when framework detected)

Each scan is O(n_instances_of_framework_class). For Hibernate: O(n_sessions) × field decode cost per session. Typical: < 100 ms per framework.

### Report size: ~10–30 KB

A few dozen framework instances × field values. Negligible.

### RSS impact: ~5 MB peak per framework scan

Field decode working set per session/pool instance. Freed after aggregation.

### ASCII art

```
FRAMEWORK ANALYSIS  (detected: Hibernate ORM · Spring Boot · HikariCP)

  HIBERNATE SESSIONS
  ┌────────────────────────────────────────────────┐
  │  12 live sessions  ·  8 have non-empty cache   │
  │  Total first-level cache retained: 890 MB  ⚠   │
  │                                                │
  │  org.hibernate.internal.SessionImpl#4821       │
  │    L1 cache: 340 MB  (4,200 entities)          │
  │  org.hibernate.internal.SessionImpl#9012       │
  │    L1 cache: 210 MB  (1,800 entities)          │
  └────────────────────────────────────────────────┘

  HIKARICP CONNECTION POOLS
  ┌──────────────────────────────────────────────┐
  │  2 pools  ·  total connections: 48           │
  │  Pool "HikariPool-1"  maxSize=20  active=18  │
  │    retained: 320 MB                          │
  └──────────────────────────────────────────────┘

  THREAD POOL EXECUTORS
  ┌──────────────────────────────────────────────┐
  │  5 pools  ·  3 have queued tasks             │
  │  "http-exec-pool"  queue depth=1,204  ⚠      │
  │    queued Runnables retained: 180 MB         │
  └──────────────────────────────────────────────┘
```

### Flag: always-on (each scan conditional on framework presence)

### Libraries: existing field-decode infrastructure

---

## 19. V20 — Two-Dump Type-Graph Diff (TPFG Diff)

### Value

**★ High conceptual interest, low practical impact until V13 ships.**

Cork §5 "temporal comparison". dotMemory "Traffic View" design. When comparing two dumps, diff the type-level reference graph (V13): show which type-edges grew in edge count or retained weight. A growing edge "RequestHandler → HttpSession" means more RequestHandlers are holding more Sessions alive — a directional leak signal.

Critique: **Requires V13 first.** V13 is itself a medium-effort backend addition. V20 is an extension of V13 for the diff case. The combined effort is L. The practical value over V12 (retained growth diff) is limited — V12 already shows which classes grew. V20 adds directional edge information (which class is responsible for the growth). Useful for complex leaks where multiple classes grow together; overkill for typical production leaks.

**Recommendation**: Defer until V12 and V13 are shipped and user feedback confirms demand for edge-level diff analysis.

### Implementation cost: L (requires V13 + diff infrastructure extension: 2 days total)

### Generation cost: medium (two TPFG builds + diff pass)

### Report size: small (~20 KB diff)

### RSS impact: ~100 MB peak (two TPFG working sets simultaneously)

### ASCII art

```
TYPE GRAPH DIFF  (Dump A → Dump B)

  Edge                          Count A  Count B   Δ count   Δ retained
  RequestHandler → HttpSession   1,200   3,420     +2,220    +340 MB  ▲▲▲
  HttpSession → HashMap          1,200   3,418     +2,218    +318 MB  ▲▲▲
  HashMap → Entry[]              1,200   3,418     +2,218    +290 MB  ▲▲▲
  String → byte[]               80,000  90,000    +10,000    +12 MB   ▲
  Object → null                 22,000  22,001        +1    < 1 MB    ≈

  ▲▲▲ = Growing edge = leak candidate.
```

### Flag: diff mode only, requires `--obj-graph`

---

## 20. New Views from Research (not yet in views-plan.md)

### V22 — Thread Stack Depth Histogram

**Value: ★ (trivial, do only if time permits)**

A histogram of thread stack depth from `ThreadInfo.frames`. Deep stacks (> 100 frames) may indicate recursion. This is a 30-minute frontend job reading existing data. Not worth its own sprint slot — fold into the Thread Retention Table (V2) as an extra column.

---

### V24 — Null-Referent Tracking for Weak/Soft References

**Value: ★★ Detects reference queue not being drained**

Count `WeakReference`, `SoftReference`, `PhantomReference` instances whose `referent` field is null (key already collected, reference not yet enqueued or processed). High null-referent count = reference queue not being drained = memory not being freed promptly.

Currently `references.weak.referent_histogram` exists. Need null-count as a separate signal.

**Impl: S (backend small addition)** | **Generate: none** | **Report size: ~2 KB** | **RSS: none**

---

### V25 — Object Graph Diameter / Longest Dominator Chain

**Value: ★★ when promoted to V1 Executive Summary; ★ as a standalone view**

The longest path in the dominator tree (by node count, computed via DFS over `idom[]`). **This is not worth a dedicated section** — it's a single scalar. Instead, surface it in the V1 Executive Summary Card as: "Longest dominator chain: 48,203 nodes (linked-list structure detected)". A chain > 10,000 nodes is a strong signal of a linked-list or deque-shaped data structure, relevant for GC pause anomalies and `StackOverflowError` investigation.

**Implementation**: DFS over `idom[]` during `build_model`, O(n) — add as a field to `HeapComposition` or `DominatorAnalysis`. No new view needed; promote to V1 card.

**Impl: S (trivial DFS + one field)** | **Generate: negligible** | **Report size: ~4 bytes** | **RSS: none**

---

### V26 — Triage Hint Signals (contextual, not a dedicated view)

**Value: ★★ as inline hints, folded into the relevant sections**

Instead of a prominent "OOM type" badge, surface each signal as a small contextual hint in the section where it's most relevant. The user doesn't get a definitive classification — they get a nudge toward the right section.

| Signal | Where shown | Hint text |
|--------|------------|-----------|
| `retention_concentration.top1_bp > 5000` | V1 Summary / Histogram | "One class holds >50% of retained heap — likely a data structure or cache leak" |
| `heap_composition.garbage_pct > 40` | V1 Summary / GC stats | "High garbage ratio — GC may be struggling to keep up; check for allocation storms" |
| `duplicate_classes.len > 50` + many classloaders | Classloader section (V7) | "Many duplicate class definitions detected — possible classloader leak" |
| `threads.len > 500` | Thread section (V2) | "Unusually high thread count — possible thread pool misconfiguration" |
| `direct_byte_buffer_capacity_sum > heap_size * 0.5` | V6 card | "Off-heap NIO memory exceeds 50% of JVM heap — likely cause of native OOM" |

Each hint is a single line shown conditionally when its threshold is crossed. No heuristic "OOM type" label — just a signal + a sentence pointing in the right direction. Implementation: add a `triage_hints: Vec<TriageHint>` field (or inline conditions in the frontend) with `{section, message}` pairs.

**Impl: S** | **Generate: negligible** | **Report size: ~100 bytes** | **RSS: none**

### Sprint 1 — Frontend only, data already present (1–2 weeks)

All S-effort items that need only UI work:

| # | View | Files | Effort |
|---|------|-------|--------|
| V1 | Executive Summary Card (+ V25 chain depth + V26 triage hints) | App.tsx | S |
| V2 | Thread Retention Table (+ V22 stack depth column) | App.tsx | S |
| V6 | DirectByteBuffer Card | App.tsx | S |
| V7 | Classloader Heatmap | App.tsx | S |
| V8 | Lambda Grouper | App.tsx | S |
| V9 | Collection Waste Budget | App.tsx (data under --collections) | S |

### Sprint 2 — Small backend additions + M-effort frontend (2–3 weeks)

| # | View | Files | Effort | Notes |
|---|------|-------|--------|-------|
| V10 | Alloc Site Flamegraph | App.tsx + charts.tsx | M | hidden when data absent |
| V5 | Who Holds This Class + Sankey drill-down | App.tsx | M | backend done (`ImmDomPair` + `pairs`); add d3-sankey UI |
| V15 | GC Root × Class Table | report/build.rs + App.tsx | S | |
| V18 | Reference Chain Visualisation | domTree.tsx + App.tsx | M | d3-sankey (group sankey) + existing TreeSvg (single chain) |
| V12 | Cross-Dump Retained Diff | report/model.rs + diff_reports.rs + App.tsx | M | diff mode only |
| V24 | Null-Referent Tracking | report/build.rs + App.tsx | S | |

**Note**: install d3-sankey + d3-force at the start of Sprint 2 (`npm install d3-sankey d3-force`) — both views use them.

### Sprint 3 — Object Graph Explorer (the centrepiece) (2–3 weeks)

| # | View | Files | Effort | Notes |
|---|------|-------|--------|-------|
| V3+V4 | Reference Graph + Dominator Tree Explorer (combined) | pass2/model.rs ✓, main.rs ✓, report/model.rs, report/build.rs, App.tsx | M | `--obj-graph`; two-tab layout, shared ObjGraphFlat data |
| V13 | Type-Level Reference Graph (Sankey + Force + Table) | report/build.rs (new aggregation pass), App.tsx | M | `--obj-graph`; d3-sankey + d3-force; interactive force graph |

### Sprint 4 — Backend-heavy additions (2–3 weeks)

| # | View | Files | Effort | Notes |
|---|------|-------|--------|-------|
| V16 | ThreadLocal Leak Analyzer | pass2/thread_locals.rs, report/model.rs, App.tsx | M | `--full-analysis` |
| V19 | Framework Auto-Analysis | pass2/framework_scan.rs, report/model.rs, App.tsx | M | conditional on framework class detection |

### Sprint 5 — Diff extensions (1 week)

| # | View | Files | Effort | Notes |
|---|------|-------|--------|-------|
| V20 | TPFG Diff (requires V13) | diff_reports.rs + App.tsx | L | `--obj-graph` + diff mode |

---

## 22. Flag Reference

| Flag | Views gated | Rationale |
|------|-------------|-----------|
| (always-on) | V1,V2,V5,V6,V7,V8,V10,V12,V15,V18,V19,V22,V24,V25,V26 | Data present; no size cost |
| `--collections` | V9 | Collection analysis is opt-in (Tier-B scan) |
| `--obj-graph` | V3,V4,V13,V20 | Adds up to ~3 MB to report; opt-in for advanced users |
| `--ref-paths` | V3 edge labels, V18 edge labels | ~100–500 MB extra RSS during analysis |
| `--full-analysis` | V16 | Tier-B field-decode scans |
| diff mode | V12, V20 | Only meaningful with two dumps |

---

## 23. Size Budget Summary

| View set | JSON addition | Compressed |
|----------|--------------|-----------|
| Sprint 1 (pure UI) | 0 | 0 |
| Sprint 2 (small backend) | ~50 KB | ~10 KB |
| V3+V4 Object Graph Explorer | ~3 MB raw (typical) | ~250 KB |
| V3+V4 worst case (200k cap) | ~50 MB raw | ~2 MB |
| V5 Immediate-Dominator | ~1.6 MB raw | ~80 KB |
| V12 Diff retained | ~30 KB | ~5 KB |
| V13 Type-Level Graph | ~50 KB | ~5 KB |
| V15 GC Root × Class | ~5 KB | ~1 KB |
| V16 ThreadLocal Leak | ~5 KB | ~1 KB |
| **Total all features** | **≈ 5 MB raw** | **≈ 400 KB typical / 2.5 MB worst case** |

All additions are within the 2 MB compressed budget for typical production heaps.

---

## 24. RSS Budget Summary

| Phase | Peak RSS addition | Freed when |
|-------|------------------|-----------|
| ObjGraphCapture (capture_obj_graph_edges) | ~30 MB | after build_model |
| BFS for ObjGraphFlat | < 10 MB | after build_model |
| TPFG aggregation (V13) | ~2 MB | after aggregation pass, freed before report emit |
| ThreadLocal scan (V16) | ~10 MB | after scan pass |
| Inner class orphan (V23) | ~20 MB | after analysis pass |
| TPFG Diff (V20) | ~100 MB | after diff |

All peaks are well under typical JVM analysis machine RAM (8–32 GB). The largest addition (TPFG Diff at ~100 MB) is gated behind `--obj-graph` + diff mode, used only by power users.

---

## 25. Critical Critique Summary

| View | Decision | Honest assessment |
|------|----------|-----------------|
| V1 | ✓ Sprint 1 | Implement first. Highest value/effort ratio. Fold in V25 chain depth. |
| V2 | ✓ Sprint 1 | Trivial upgrade. Netflix case study alone justifies it. Fold V22 stack depth as a column. |
| V3+V4 | ✓ Sprint 3 (`--obj-graph`) | The centrepiece. Backend capture already implemented. Two-tab combined view. |
| V5 | ✓ Sprint 2 | #1 SO question. Two-sided sankey pivot. Backend done; d3-sankey frontend is the only work. |
| V6 | ✓ Sprint 1 | Trivial, prevents expensive misdiagnosis of native memory leaks. |
| V7 | ✓ Sprint 1 | Trivial wiring. Classloader leaks invisible without it. |
| V8 | ✓ Sprint 1 | Lambda grouping quality-of-life. Smart `$$Lambda$NNN` regex, toggle to raw names. |
| V9 | ✓ Sprint 1 | S-effort, client-side aggregation. No fix suggestions — link external docs per waste type. |
| V10 | ✓ Sprint 2 | Rare but high-value when present. Hidden when `alloc_sites.traces_present = false`. |
| V12 | ✓ Sprint 2 (diff) | High value for two-dump workflow. Retained deltas far more useful than shallow deltas. |
| V13 | ✓ Sprint 3 (`--obj-graph`) | Three-view design (sankey + force + table). d3-force runs in browser, fully interactive. |
| V15 | ✓ Sprint 2 | Small backend change, directly diagnoses JNI leaks. |
| V16 | ✓ Sprint 4 (`--full-analysis`) | YourKit's most-used inspection. New ThreadLocalMap field-decode scan pass. |
| V18 | ✓ Sprint 2 | SVG chain (single suspects) + d3-sankey (group merged paths). Both are frontend-only. |
| V19 | ✓ Sprint 4 | Upgraded: auto-run framework analyses at report time when classes detected. Proactive, not query suggestions. |
| V20 | ✓ Sprint 5 (after V13) | Requires V13. Directional leak signal (edge-level diff). Defer until V13 validated. |
| V22 | ✓ Sprint 1 (folded) | Fold stack depth as a column in V2. Not a standalone view. |
| V24 | ✓ Sprint 2 | Trivial backend addition. Null-referent count as a triage signal alongside referent histogram. |
| V25 | ✓ Sprint 1 (folded) | Single scalar: longest dominator chain. Surface in V1 Executive Summary card. |
| V14 | ✗ skipped | Soft-ref pressure gauge. Data exists but signal too often low/uninformative. |
| V17 | ✗ skipped | Dominator sunburst. Treemap already covers this; sunburst is extra complexity for marginal value. |
| V21 | ✗ skipped | String interning ROI. Duplicate strings section already covers this. |
| V23 | ✗ skipped | Inner class orphan detector. Less relevant for modern server-side Java. |
| V26 | ✓ Sprint 1 (folded) | Triage hints: contextual single-line signals in the relevant section (histogram, GC, threads, V6, V7). Not a dedicated view, not an OOM badge. |

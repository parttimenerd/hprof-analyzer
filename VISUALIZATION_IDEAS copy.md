# hprof-analyzer — Heap Visualization Ideas

Comprehensive catalog of new views and charts to help users discover *what* uses
heap memory and *why*. Each idea is judged on value, feasibility, and fit with
the existing codebase.

**Key principle: zero new npm dependencies.** Every proposed visualization is
achievable with the libraries already installed in `web/package.json`. The
previous draft recommended Nivo (~390 KB gz); this revision eliminates that
recommendation entirely after auditing each chart against existing capabilities.

**Working demos:** [`docs/visualization-demos.html`](docs/visualization-demos.html)
contains interactive prototypes of the top 10 ideas, all built with existing
libraries (d3-hierarchy, d3-sankey, Chart.js, pure SVG/CSS).

---

## Table of Contents

1. [Current State Baseline](#current-state-baseline)
2. [Library Audit](#library-audit) — what we have vs. what we need
3. [Idea Catalog](#idea-catalog) — 35 ideas in 9 categories
   - [A — Dominator & Ownership Structure](#a--dominator--ownership-structure) (5 ideas)
   - [B — Flow & Path Views](#b--flow--path-views) (4 ideas)
   - [C — Histogram & Distribution Views](#c--histogram--distribution-views) (4 ideas)
   - [D — Collection & Waste Views](#d--collection--waste-views) (6 ideas)
   - [E — Thread & Concurrency Views](#e--thread--concurrency-views) (2 ideas)
   - [F — ClassLoader & Framework Views](#f--classloader--framework-views) (2 ideas)
   - [G — Diff & Temporal Views](#g--diff--temporal-views) (3 ideas)
   - [H — Summary & Decision Views](#h--summary--decision-views) (2 ideas)
   - [I — Currently Unvisualized Data](#i--currently-unvisualized-data) (4 ideas)
4. [Suggested New JSON Data](#suggested-new-json-data) — Rust backend additions
5. [Implementation Roadmap](#implementation-roadmap)
6. [Verdict Summary Table](#verdict-summary-table)

---

## Current State Baseline

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

### Completely unvisualized data (already in report JSON)

These data sources are emitted by the Rust backend but have **zero frontend
rendering** today. They are the highest-value targets:

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

---

## Library Audit

### Decision: use what we have — zero new packages

After auditing each proposed chart against the installed packages, **every idea
in this document is achievable without installing anything new**. The previous
draft recommended Nivo (~390 KB gz); this revision eliminates that.

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

### Chart type → library mapping (no new packages)

| Chart type | Library | How | Bundle cost |
|-----------|---------|-----|:-----------:|
| **Sunburst** | `d3-hierarchy` `partition()` | `partition().size([2π, radius])` → manual SVG arc paths. ~40 LOC of arc math (see demo). | 0 KB |
| **Sankey** | `d3-sankey` (installed) | Layout-only → render SVG paths + rects manually. ~80 LOC. | 0 KB |
| **Heatmap** | Pure CSS | HTML `<table>` + `rgba()` cell backgrounds. ~30 LOC. No library needed at all. | 0 KB |
| **Chord** | ~~`@nivo/chord`~~ | **Dropped.** Heatmap (#A2) serves the same purpose more readably. | — |
| **Scatter** | `chart.js` (installed) | Register `ScatterController` + `PointElement` in `chartSetup.ts` (+2 imports, +2 in register call). Already in the chart.js bundle. | 0 KB |
| **Waterfall** | Pure SVG | Manual `<rect>` elements. ~50 LOC. | 0 KB |
| **Sparklines** | Pure SVG `<polyline>` | 10–15 lines of code per sparkline. | 0 KB |
| **Flame graph** | Adapt `ZoomableTreemap` | Existing flame/icicle toggle already in `ZoomableTreemap`. Adapt for `AllocSite[]` tree. | 0 KB |
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

**Effort:** ~80 LOC (60 chart + 20 breadcrumbs/state).

---

#### A2. Who-Holds-Whom Heatmap Matrix 🟢

| | |
|-|-|
| **Question** | Which class pairs have the strongest dominator relationships across the whole heap? |
| **Data** | `DominatorAnalysis.immediate_dominators.pairs: ImmDomPair[]` (always-on, up to 20K pairs) |
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

**Pros:**
- Makes cross-retention anomalies visible at a glance — "why is `byte[]` retained by `ResponseBuffer`?"
- Data is already there — `ImmDomPair` has `dominator_class`, `dominated_class`, `dominated_retained`
- Clicking a cell can navigate to the dominated class in the histogram
- Pure CSS — zero dependency cost

**Cons:**
- Top 20×20 matrix = 400 cells, manageable; top 50×50 = 2500, needs virtualization
- Asymmetric labels (long Java class names) require truncation + title tooltip
- The heatmap and the chord diagram serve similar purposes — we're choosing heatmap (more readable)

**Performance:** Renders 400 HTML `<td>` elements. ~1 ms. JSON unchanged.

**Memory impact:** ~100 KB DOM for a 20×20 matrix with tooltips. Negligible.

**Effort:** ~50 LOC.

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

**Effort:** ~50 LOC.

---

### B — Flow & Path Views

*These views answer: "HOW does memory flow from GC roots to the big consumers?"*

#### B1. Retention Flow Sankey 🟢

| | |
|-|-|
| **Question** | How does memory flow from GC root categories through container classes to primitive arrays? |
| **Data** | Primary: `TypeEdge[]` from `type_ref_graph` (opt-in). Fallback: `GcRootRetainedRow[]` (always-on). |
| **Library** | `d3-sankey` (already installed) + manual SVG rendering — **zero new packages** |
| **Where** | New standalone section, or inside Dominator Analysis. |
| **Demo** | [`docs/visualization-demos.html#demo-sankey`](docs/visualization-demos.html#demo-sankey) |

**How it works with existing libraries:**

`d3-sankey` is already in `package.json`. It provides layout-only:
```typescript
import { sankey, sankeyLinkHorizontal, sankeyLeft } from 'd3-sankey';

const layout = sankey()
  .nodeWidth(18).nodePadding(14)
  .nodeAlign(sankeyLeft)
  .extent([[1, 20], [W - 1, H - 20]]);
const graph = layout({ nodes, links });
```
Then render SVG manually — `<rect>` for nodes, `<path d={sankeyLinkHorizontal()(link)}>` for links.
The demo proves this works. The previous draft recommended `@nivo/sankey` (55 KB)
for "turnkey hover, link gradients, label truncation" — but the SVG rendering is
~80 LOC and gives full control over theming, hover behavior, and label placement.

**Pros:**
- **No existing Java heap tool renders this view** — genuinely novel
- `TypeEdge[]` is *literally* a weighted directed graph — zero data transformation needed
- The fallback (`GcRootRetainedRow[].top_classes`) ensures it works without `--obj-graph`
- Shows aggregate retention flow in one picture: "60% flows from threads → HashMap → Entry → byte[]"

**Cons:**
- `TypeEdge[]` is capped at 500 edges — the Sankey only shows the top-500 class pairs
- With the fallback data, the Sankey is only 2 layers deep (root type → top class)
- Sankey diagrams can be confusing to users unfamiliar with the chart type
- SVG link hover requires manual event handlers (~15 extra LOC vs. Nivo)

**Performance:** ~50 SVG `<rect>` nodes + ~100 `<path>` links. ~3 ms layout, ~5 ms render.
JSON unchanged.

**Memory impact:** ~80 KB SVG DOM for a full 500-edge Sankey. Acceptable.

**Effort:** ~120 LOC (layout + SVG + hover + labels). More than the 40 LOC Nivo version, but
zero dependency cost and full theme control.

---

#### B2. Blame Propagation Bars 🟢

| | |
|-|-|
| **Question** | Through which reference edges does the most retained memory flow? |
| **Data** | `Suspect.merged_paths: MergedPathNode` (recursive, with `retained` and `field_edge` per node) |
| **Library** | None — pure React + CSS inline bars |
| **Where** | Leak Suspects section, replacing the plain merged-path tree renderer. |
| **Demo** | [`docs/visualization-demos.html#demo-blame`](docs/visualization-demos.html#demo-blame) |

**How it works:**

Each node in the merged path tree shows a proportional bar:
```
blame = child.retained / parent.retained
barColor = blame > 0.7 ? red : blame > 0.3 ? amber : green
```
Red = this edge carries >70% of the parent's retained heap (hot path).
Green = <30% (cold path). This replaces the plain tree renderer with
zero extra dependencies.

**Pros:**
- `blame = child.retained / parent.retained` is trivially computed — zero backend work
- Visually highlights the 80% path vs. the 20% path, directly guiding fix priority
- No existing tool does proportional blame quantification on retention chains

**Cons:**
- Only available for class-group suspects (which have `merged_paths`). Single suspects have `root_path` instead.
- The merged path tree can be deep (20+ nodes) — needs collapsible rendering

**Performance:** ~20–50 inline CSS bars. Trivial. JSON unchanged.

**Memory impact:** Negligible.

**Effort:** ~60 LOC (replaces existing renderer).

---

#### B3. GC Root Retained-by-Type Breakdown 🟢

| | |
|-|-|
| **Question** | How much heap is pinned by static fields vs. thread locals vs. JNI — and what classes are inside each? |
| **Data** | `SystemOverview.gc_roots_retained_by_type: GcRootRetainedRow[]` (always-on, includes `top_classes`) |
| **Library** | `FlatTreemap` (existing) + expandable sub-bars |
| **Where** | System Overview, near GC roots section. |

**Pros:**
- Directly answers "is this a static-field leak or a thread-local leak?" before reading suspects
- `top_classes` per root type shows the *specific classes* pinned by each root category
- Data is always-on — no opt-in flag required

**Cons:**
- May overlap with the Sankey (#B1) which also uses this data as fallback input
- Typically 5–8 root types — may look sparse as a standalone chart

**Performance:** 5–8 treemap tiles + expandable rows. Trivial. JSON unchanged.

**Memory impact:** Negligible.

**Effort:** ~70 LOC.

---

#### B4. NIO Direct Buffer Attribution Tree 🟢

| | |
|-|-|
| **Question** | Which objects own the largest off-heap DirectByteBuffer allocations, and what GC root chain keeps them alive? |
| **Data** | `LeakIndicators.{direct_byte_buffer_cap_buckets, nio_buffer_owners: NioBufferOwner[]}` (always-on) |
| **Library** | `VBar` (existing) + existing `RootPathStep` path renderer |
| **Where** | System Overview or a dedicated "Off-Heap" panel. |

**Pros:**
- DirectByteBuffers are the #1 cause of native OOM — this data is critical but **completely invisible** today
- `nio_buffer_owners[]` includes full GC root paths — immediately actionable
- Capacity histogram (8 log-scale buckets) gives a quick shape overview

**Cons:**
- `nio_buffer_owners` may be null/empty on dumps without NIO usage
- Off-heap capacity is separate from retained heap — mixing the two in one view could confuse users

**Performance:** 8 bars + 5–10 expandable path trees. Trivial. JSON unchanged.

**Memory impact:** Negligible.

**Effort:** ~60 LOC.

---

### C — Histogram & Distribution Views

*These views answer: "What is the SHAPE of the heap — how many objects, how big, how distributed?"*

#### C1. Category Budget Treemap 🟢

| | |
|-|-|
| **Question** | How much heap belongs to my app code vs. frameworks vs. JDK internals? |
| **Data** | `Report.category_rollup: CategoryRollup[]` (always-on) |
| **Library** | `FlatTreemap` (existing) |
| **Where** | System Overview, as a "Who owns this heap?" row above the histogram. |
| **Demo** | [`docs/visualization-demos.html#demo-budget`](docs/visualization-demos.html#demo-budget) |

**Pros:**
- **Data is already emitted and never rendered** — literally 15 lines of frontend code
- Answers the first question every product engineer asks: "Is it Spring? Is it us?"
- Clicking a tile filters the histogram to that category (reuse existing filter buttons)

**Cons:**
- Only 3–4 categories (jdk / framework / other / custom) — the treemap may look overly simple
- The category classifier can be imprecise for obscure libraries

**Performance:** 3–4 treemap tiles. Trivial. JSON unchanged.

**Memory impact:** Negligible.

**Effort:** ~15 LOC. **Lowest-cost, highest-value item in the entire catalog.**

---

#### C2. Framework Footprint Panel 🟢

| | |
|-|-|
| **Question** | How much heap is each detected framework consuming? |
| **Data** | `Report.framework_analysis: FrameworkAnalysis[]` (always-on when sentinel classes present) |
| **Library** | `HBar` (existing) or table + inline bar |
| **Where** | System Overview, alongside or below category budget. |
| **Demo** | [`docs/visualization-demos.html#demo-frameworks`](docs/visualization-demos.html#demo-frameworks) |

**Pros:**
- `framework_analysis` is **completely unvisualized** today
- Per-framework retained bytes directly answers "Is it Spring (40 MB) or Hibernate (120 MB)?"
- Data is always-on — zero opt-in flags

**Cons:**
- Empty when no known frameworks are detected (pure JDK + custom code)
- Framework detection is heuristic-based — may miss uncommon frameworks

**Performance:** 3–8 horizontal bars. Trivial. JSON unchanged.

**Memory impact:** Negligible.

**Effort:** ~30 LOC.

---

#### C3. Fat-Class Scatter Plot 🟢

| | |
|-|-|
| **Question** | Which classes have surprisingly large per-instance memory footprints? |
| **Data** | `HistRow[]` — needs new `avg_shallow_bytes` field (see [D-1](#d-1-histrow-average-sizes--heap-share-percent)) |
| **Library** | `chart.js` `ScatterController` (already in bundle — just register it) |
| **Where** | System Overview, as an alternative view of the class histogram. |

**How it works:**

Chart.js includes `ScatterController` and `PointElement` in its bundle. Register
them in `chartSetup.ts` (see [setup above](#scatter-plot-one-time-setup-in-chartsetupts)),
then use `react-chartjs-2`'s `<Scatter>` component:
```tsx
<Scatter data={{
  datasets: [{ data: histRows.map(r => ({ x: r.instances, y: r.avg_shallow })) }]
}} options={{ scales: { x: { type: 'logarithmic' }, y: { type: 'logarithmic' } } }} />
```
The Canvas renderer handles 5000+ points at 60fps. No Nivo needed.

**Pros:**
- Scatter plot of `instances` (x) vs `avg_shallow_bytes` (y) immediately highlights outlier classes
- A class with 10 instances but 5 MB average shallow is a different problem than 10M instances at 32 bytes
- No existing tool visualizes average-instance-size distribution
- Chart.js Canvas renderer handles large point counts well

**Cons:**
- **Requires new JSON field** (`avg_shallow_bytes`) — trivial derivation in Rust but still a backend change
- Log-scale axes are needed for heaps with both tiny and huge classes — `LogarithmicScale` is already registered
- Scatter plots are less familiar than bar charts for heap analysis

**Performance:** Canvas-based → handles 5000+ points at 60fps. No concern. JSON grows by ~4 bytes per HistRow.

**Memory impact:** Canvas is memory-efficient vs. SVG for large point counts. ~2 MB Canvas for 5K points.

**Effort:** ~70 LOC frontend + ~8 LOC Rust.

---

#### C4. Object Size Distribution Histogram 🟢

| | |
|-|-|
| **Question** | Are most objects tiny (< 64 bytes) or are there surprisingly fat instances? |
| **Data** | `TopConsumers.size_distribution: TopSizeDistribution` + `ArraysBySize` (both always-on) |
| **Library** | `VBar` (existing) |
| **Where** | System Overview or Top Consumers. |

**Pros:**
- Both `TopSizeDistribution.buckets` and `ArraysBySize` are **always-on and never charted**
- Understanding object-size distribution is fundamental to GC tuning decisions
- `arrays_by_size` reveals whether large arrays dominate (a very different fix than many tiny objects)

**Cons:**
- The `SizeBucket` format is already bucketed — no additional aggregation needed, but the bucket boundaries may not be intuitive
- Dual-panel (object sizes + array lengths) may feel like two charts that should be one

**Performance:** ~30 bars total. Trivial. JSON unchanged.

**Memory impact:** Negligible.

**Effort:** ~60 LOC.

---

### D — Collection & Waste Views

*These views answer: "WHERE is memory being WASTED — on empty collections, duplicate strings, oversized arrays?"*

#### D1. Waste Waterfall / Treemap 🟢

| | |
|-|-|
| **Question** | How much memory is reclaimable without any functional change, and what are the sources? |
| **Data** | `Report.waste_summary: WasteSummary { total_bytes, sources: WasteSource[] }` (always-on when waste > 0) |
| **Library** | `FlatTreemap` (existing) or stacked bar |
| **Where** | Triage section, as a visual companion to the triage signals. |
| **Demo** | [`docs/visualization-demos.html#demo-waste`](docs/visualization-demos.html#demo-waste) |

**Pros:**
- `WasteSummary` is **present but never rendered as a chart** — only referenced in triage text
- A treemap of waste sources gives an instant "bang for your buck" overview
- Each `WasteSource.anchor` links to the relevant report section — the chart doubles as navigation

**Cons:**
- `waste_summary` may be absent (null) when all waste sources are zero
- Waste sources may overlap slightly — the total is approximate

**Performance:** 3–8 treemap tiles. Trivial. JSON unchanged.

**Memory impact:** Negligible.

**Effort:** ~40 LOC.

---

#### D2. Collection Capacity vs. Usage Scatter 🟢

| | |
|-|-|
| **Question** | Which individual collection instances waste the most capacity? |
| **Data** | `BiggestCollections.combined: BiggestCollectionRow[]` (always-on with `--collections`) |
| **Library** | `chart.js` `ScatterController` (already in bundle) |
| **Where** | Collections section. |

**Pros:**
- Per-instance `elements` (usage) vs `retained` (cost) scatter immediately surfaces outliers
- The `owner: string` field labels each dot with its `Class#field` holder
- YourKit has a similar per-instance waste view — consistently the highest-value output for memory waste

**Cons:**
- Requires `--collections` flag (not always-on)
- `BiggestCollectionRow` uses `elements` not `capacity` — retained serves as a proxy but isn't identical
- Needs diagonal "ideal line" overlay to show expected element→retained relationship

**Performance:** Typically 50–200 points (biggest collections are capped). Fine for Canvas.

**Memory impact:** Negligible.

**Effort:** ~80 LOC.

---

#### D3. String Universe Content Classifier 🟢

| | |
|-|-|
| **Question** | What *kind* of strings are burning memory — URLs, SQL, JSON, UUIDs, class names? |
| **Data** | `DupStrings.top_duplicated: DupStringSample[]` (opt-in with `--find-duplicates`) |
| **Library** | `FlatTreemap` (existing) |
| **Where** | Duplicate Strings section, as a new "By Content Type" tab. |

**Pros:**
- Goes beyond "how many duplicates?" to "what kind of duplicates?" — directly identifies the subsystem to fix
- Pure frontend regex classification over string samples already in the report
- "40% of wasted string bytes are HTTP headers" → fix the serialization layer

**Cons:**
- Regex classification is heuristic — some strings won't match any category
- Only samples the top-duplicated strings, not all strings — coverage depends on `--find-duplicates` depth

**Performance:** ~20 regex tests × ~200 string samples. Trivial. JSON unchanged.

**Memory impact:** Negligible.

**Effort:** ~70 LOC.

---

#### D4. Tiny-Collection Overhead Lollipop 🟢

| | |
|-|-|
| **Question** | Which Class#field combinations burn the most bytes on empty/singleton collections that should be lazy-initialized? |
| **Data** | `CollectionAttribution.tiny_overhead: TinyCollectionRow[]` (opt-in with `--collections`) |
| **Library** | Table + inline CSS bar (or pure SVG lollipop) |
| **Where** | Collections section. |

**Pros:**
- `TinyCollectionRow` is **completely unvisualized** — one of the most actionable waste signals
- The fix is mechanical (lazy init pattern) and the savings are calculable (`overhead_bytes`)
- Sorted by overhead_bytes, the top 5 rows are directly copy-pasteable into a PR

**Cons:**
- Requires `--collections` flag
- Lollipop charts are less familiar — a simple table with a bar column works just as well

**Performance:** 10–30 table rows. Trivial. JSON unchanged.

**Memory impact:** Negligible.

**Effort:** ~70 LOC.

---

#### D5. Collection Type Composition Matrix 🟡

| | |
|-|-|
| **Question** | What types do Maps/Lists/Sets actually contain? |
| **Data** | `CollectionContents.rows[].top_value_types: ValueTypeShare[]` (opt-in with `--collections`) |
| **Library** | Pure CSS heatmap (same technique as A2) or table |

**Pros:**
- Shows which collections hold application objects vs. JDK internals
- `ValueTypeShare` already has type name + count — direct input for a heatmap

**Cons:**
- Niche — most users care about collection *size*, not content type
- Requires `--collections` flag

**Performance:** ~50 cells max. Trivial.

**Memory impact:** Negligible.

**Effort:** ~60 LOC.

---

#### D6. Field Null-Rate Inspector 🟡

| | |
|-|-|
| **Question** | Which reference fields are predominantly null — wasting pointer-slot space? |
| **Data** | `Report.field_stats: FieldStats` (opt-in with `--field-stats`) |
| **Library** | Table + inline bar |

**Pros:**
- Unique metric: `null_count / (null_count + non_null_count)` per field. Not available elsewhere.
- High null rates → move the field to a secondary object or make it lazy

**Cons:**
- Requires `--field-stats` flag (heavy analysis)
- Niche audience (framework developers, not application developers)

**Performance:** ~100 table rows. Trivial.

**Memory impact:** Negligible.

**Effort:** ~80 LOC.

---

### E — Thread & Concurrency Views

#### E1. Thread State Breakdown Treemap 🟢

| | |
|-|-|
| **Question** | What are all threads doing, and how much heap does each state category retain? |
| **Data** | `ThreadOverview.threads: ThreadInfo[]` (always-on) |
| **Library** | `FlatTreemap` (existing) |
| **Where** | Threads section, as a summary above the thread table. |
| **Demo** | [`docs/visualization-demos.html#demo-threads`](docs/visualization-demos.html#demo-threads) |

**Pros:**
- Groups threads by `thread_state`, sizes tiles by sum of `retained` per state
- Instantly shows "70% of retained heap is in WAITING threads" — a strong leak signal
- Zero backend changes — all fields present

**Cons:**
- `thread_state` is a decoded string label; JVMTI sub-states are collapsed (see [D-3](#d-3-threadinfo-raw-jvmti-thread-state-bitmask))
- Thread retained sizes are approximate (based on local object sampling)

**Performance:** 4–6 treemap tiles. Trivial. JSON unchanged.

**Memory impact:** Negligible.

**Effort:** ~50 LOC.

---

#### E2. ThreadLocal Leak Analyzer View 🟢

| | |
|-|-|
| **Question** | Are stale ThreadLocal entries accumulating across pooled threads? |
| **Data** | `Report.thread_local_analysis: ThreadLocalLeakRow[]` + `leak_indicators.thread_local_null_key_count` |
| **Library** | Table + KPI badge |
| **Where** | Threads section, or a dedicated "ThreadLocal" tab. |

**Pros:**
- `ThreadLocalLeakRow` is **completely unvisualized** — the most actionable leak signal for servlet containers
- `stale_count` directly identifies entries where the owning thread is dead but the value persists
- `thread_local_null_key_count` KPI is a single number that answers "do I have a TL leak?"

**Cons:**
- Requires `--find-duplicates` flag (not always-on)
- Empty on applications that don't use ThreadLocals

**Performance:** ~20 table rows + 1 KPI badge. Trivial. JSON unchanged.

**Memory impact:** Negligible.

**Effort:** ~50 LOC.

---

### F — ClassLoader & Framework Views

#### F1. ClassLoader Topology Tree 🟡

| | |
|-|-|
| **Question** | What is the classloader hierarchy, and how much heap does each loader retain? |
| **Data** | `LoaderRollup[]` — **needs `parent_loader_id`** (see [D-7](#d-7-loaderrollup-parent-loader-id)) |
| **Library** | `d3-hierarchy tree()` (installed) or Cytoscape (installed) |

**How it works with existing libraries:**

`d3-hierarchy` provides `d3.tree()` which produces `x`, `y` coordinates for each
node. Render with SVG lines and circles — ~60 LOC. Alternatively, use Cytoscape
(already installed) with a hierarchical layout. No `@xyflow/react` needed.

**Pros:**
- Classloader hierarchy visualization is absent from every HTML-native heap tool
- Once `parent_loader_id` is added, the data drops directly into `d3.tree()`

**Cons:**
- **Requires backend change** — adding `parent_loader_id` to `LoaderRollup`
- Niche — primarily useful for JEE, OSGi, and plugin-system environments

**Performance:** ~10–30 tree nodes. Trivial.

**Memory impact:** Negligible.

**Effort:** ~60 LOC frontend + ~25 LOC Rust.

---

#### F2. Duplicate Class Clash View 🟡

| | |
|-|-|
| **Question** | Which classes are loaded by multiple classloaders, and how much does each copy cost? |
| **Data** | `SystemOverview.duplicate_classes: DuplicateClass[]` + `per_loader: DuplicateClassLoaderRow[]` |
| **Library** | Chart.js stacked bar (installed; `StackedBar` component exists) |

**Pros:**
- `duplicate_classes` is **completely unvisualized** — important for hot-redeploy leak diagnosis
- `per_loader` breakdown shows exactly how much each loader contributes

**Cons:**
- Only useful when duplicate classes exist (not common in simple applications)
- Stacked bars with 3+ loaders and long class names get crowded

**Performance:** Typically 5–15 bars. Trivial. JSON unchanged.

**Memory impact:** Negligible.

**Effort:** ~80 LOC.

---

### G — Diff & Temporal Views

#### G1. Growth Sparklines in Diff Table 🟢

| | |
|-|-|
| **Question** | Is this class growing steadily, spiking, or fluctuating across snapshots? |
| **Data** | `SeriesClassRow.retained: number[]` (N values across N snapshots) |
| **Library** | Pure SVG `<polyline>` (zero dependencies) |
| **Where** | Diff view, as an inline column in the growth-leaders table. |
| **Demo** | [`docs/visualization-demos.html#demo-sparklines`](docs/visualization-demos.html#demo-sparklines) |

**How it works:**

```tsx
function Sparkline({ values }: { values: number[] }) {
  const w = 100, h = 28;
  const min = Math.min(...values), range = Math.max(...values) - min || 1;
  const pts = values.map((v, i) =>
    `${(i / (values.length - 1)) * w},${h - ((v - min) / range) * (h - 4) - 2}`
  ).join(' ');
  const growing = values[values.length - 1] > values[0];
  return (
    <svg width={w} height={h}>
      <polyline points={pts} fill="none"
        stroke={growing ? 'var(--red)' : 'var(--green)'}
        strokeWidth={2} strokeLinejoin="round" />
    </svg>
  );
}
```

**Pros:**
- `retained[]` is already an N-element array — perfect for a sparkline
- Turns a delta column into a trend line — immediately shows steady growth vs. spike vs. sawtooth
- Zero dependencies, ~15 lines of code per sparkline
- Red for growing, green for shrinking — clear visual signal

**Cons:**
- N = 2 (two snapshots) produces a trivial two-point line — only valuable for N ≥ 3

**Performance:** ~20 sparklines × ~10 SVG points each. <1 ms. JSON unchanged.

**Memory impact:** ~5 KB SVG. Negligible.

**Effort:** ~40 LOC.

---

#### G2. Animated Diff Treemap 🟡

| | |
|-|-|
| **Question** | What changed between two snapshots — visually? |
| **Data** | `SeriesDiffResult.growth_leaders: SeriesClassRow[]` |
| **Library** | CSS `transition` on existing `FlatTreemap` divs |

**Pros:**
- Animated morph from snapshot A to snapshot B makes change viscerally clear
- CSS transitions are free — no library needed

**Cons:**
- Squarify treemaps re-arrange tiles between states, causing confusing jumps
- Only shows class-level change, not structural/reference-level change
- Sparklines (#G1) may communicate the same information more clearly

**Performance:** CSS only. No SVG/Canvas overhead.

**Memory impact:** Negligible.

**Effort:** ~40 LOC.

---

#### G3. Type-Edge Delta Table 🟡

| | |
|-|-|
| **Question** | Between snapshots, which new class→class reference relationships appeared? |
| **Data** | `SeriesDiffResult.tpfg_diff: TypeEdgeDiff[]` (requires `--obj-graph` on both dumps) |
| **Library** | Table + CSS heatmap overlay |

**Pros:**
- New edges (count_first = 0) are the earliest warning of an emerging leak
- `delta_weight` shows how much retained heap shifted along each edge

**Cons:**
- Requires `--obj-graph` flag on *both* snapshots — rare in practice
- The 500-edge cap on `TypeEdge[]` means many small-edge changes are dropped

**Performance:** ~100 table rows + small heatmap. Trivial.

**Memory impact:** Negligible.

**Effort:** ~90 LOC.

---

### H — Summary & Decision Views

#### H1. Fix ROI Priority Ranker 🟢

| | |
|-|-|
| **Question** | Where should I start fixing — which leak suspect gives the most memory back for the least effort? |
| **Data** | `LeakSuspects.suspects: Suspect[]` — `retained`, `root_type_label`, `keywords` |
| **Library** | Table with inline bars |
| **Where** | Top of Leak Suspects section, or as a standalone "Action Plan" panel. |

**Pros:**
- Transforms the tool from "analysis report" to "decision support"
- `root_type_label` maps to fix complexity: Static field = easy (1 line), ThreadLocal = medium, JNI = hard
- `ROI = retained / complexity` is trivially computed in frontend

**Cons:**
- Fix-complexity heuristic is coarse — a static field holding a 200-class graph is not "easy"
- Users may over-trust the ranking and skip important but lower-ROI suspects

**Performance:** Trivial. JSON unchanged.

**Memory impact:** Negligible.

**Effort:** ~50 LOC.

---

#### H2. Natural-Language Triage Digest 🟢

| | |
|-|-|
| **Question** | In plain English, what is wrong with this heap dump? |
| **Data** | `Report.triage: TriageSignal[]` + `waste_summary` + `leaks.suspects[0]` |
| **Library** | None — pure template-based string assembly |
| **Where** | Top of the report, above all sections. |

**Pros:**
- A 3–5 sentence summary dramatically lowers the expertise barrier
- Deterministic, no LLM — uses triage severity, suspect class, retained %, root type
- All inputs are already in the report

**Cons:**
- Template sentences can feel robotic compared to LLM-generated explanations
- Edge cases (multiple suspects, no clear culprit) require careful template branching

**Performance:** Pure string concatenation. Zero overhead.

**Memory impact:** Zero.

**Effort:** ~80 LOC (mostly template logic).

---

### I — Currently Unvisualized Data

*These data sources are in the report JSON but have zero visualization today.*

#### I1. Reference Leak Heatmap (Soft/Weak/Phantom) 🟢

| | |
|-|-|
| **Question** | Which object types survive only because a soft/weak reference holds them? |
| **Data** | `Report.references: ReferencesAnalysis` — `only_weakly_retained: RefStatClassRow[]` per kind |
| **Library** | `FlatTreemap` or `HBar` (existing) |

**Pros:**
- `ReferencesAnalysis` is **always-on** and **has zero visualization** in the entire UI
- `only_weakly_retained` is the most directly actionable field for caching bugs — these objects survive GC by luck
- `null_referent_count` identifies already-cleared-but-not-GC'd references (postmortem leak pattern)

**Cons:**
- Empty when the heap has no soft/weak/phantom references
- Three separate kinds (soft/weak/phantom) may feel like redundant sub-panels

**Performance:** 3 small bar charts or treemaps. Trivial. JSON unchanged.

**Memory impact:** Negligible.

**Effort:** ~60 LOC.

---

#### I2. Package Retention Treemap (Zoomable) 🟢

| | |
|-|-|
| **Question** | Which package subtree dominates retained heap? |
| **Data** | `TopConsumers.biggest_packages: PackageNode` (always-on, recursive) |
| **Library** | `ZoomableTreemap` (existing — **already consumes this exact data shape**) |

**Pros:**
- `PackageNode` is a recursive tree with `retained_heap` and `children[]` — identical shape to what `ZoomableTreemap` already accepts
- **~20 LOC wiring** to add a "By Package" toggle alongside the existing "By Class" view
- Lets engineers navigate from `com.example` → `.service` → `.cache` without knowing class names

**Cons:**
- The package tree may be very deep (10+ levels) — `ZoomableTreemap` handles this with breadcrumbs

**Performance:** Same as existing treemap. JSON unchanged.

**Memory impact:** Same as existing treemap.

**Effort:** ~20 LOC. **Second lowest-cost item after C1.**

---

#### I3. Unreachable / Garbage Object Composition 🟡

| | |
|-|-|
| **Question** | What is in the garbage — is it dominated by one pattern or scatter? |
| **Data** | `SystemOverview.unreachable_histogram` + `unreachable_garbage_roots` + `unreachable_composition` |
| **Library** | `FlatTreemap` + `DomSubtreeSvg` (existing) |

**Pros:**
- Three dedicated unreachable-objects structures exist and **none are visualized**
- Large unreachable heap with a single dominant garbage root = GC throughput problem, not a leak

**Cons:**
- Secondary to reachable heap analysis — most users care about live objects first
- `unreachable_garbage_roots` may be null (dominator computation skipped for unreachable objects)

**Performance:** Small treemap + tree. Trivial.

**Memory impact:** Negligible.

**Effort:** ~90 LOC.

---

#### I4. Allocation Flame Graph 🟡

| | |
|-|-|
| **Question** | Where in the code were the most-retained objects allocated? |
| **Data** | `Report.alloc_sites: AllocSites { traces_present, sites: AllocSite[] }` |
| **Library** | Adapt existing `ZoomableTreemap` flame/icicle mode — **no `d3-flame-graph` needed** |

**How it works with existing components:**

`ZoomableTreemap` already has a flame/icicle toggle. `AllocSite[]` has `frames[]`
(call stack) + `retained_total`. The steps:
1. Merge common stack prefixes into a tree (simple trie construction, ~30 LOC)
2. Feed the tree to `ZoomableTreemap` in flame mode

This reuses 100% existing UI code. The previous draft recommended `d3-flame-graph`
(40 KB gz) — unnecessary.

**Pros:**
- `AllocSite` has `frames[]` + `retained_total` — exactly the flame graph input
- Bridges "what is retained" (dominator tree) with "where was it created" (allocation site)
- Reuses existing `ZoomableTreemap` — consistent UX

**Cons:**
- **HPROF allocation traces are rare** — most dumps are captured with `jcmd GC.heap_dump`, which doesn't include them. `traces_present` is almost always `false`.
- When present, the data is a flat list of `AllocSite[]`, not a tree — must be folded into a tree by merging common stack prefixes (~30 LOC)

**Performance:** Depends on number of unique stacks (typically <500). Fine.

**Memory impact:** Same as existing treemap.

**Effort:** ~80 LOC (30 LOC trie + 50 LOC wiring to existing component).

---

## Suggested New JSON Data

Additions to the Rust backend that unlock new visualizations. Ordered by cost.

#### D-1. `HistRow`: average sizes + heap-share percent

| | |
|-|-|
| **Fields** | `avg_shallow_bytes: u64`, `avg_retained_bytes: u64`, `shallow_pct_bp: u32` |
| **Rust file** | `src/report/build.rs` (inside `build_system_overview`, hist loop) |
| **Cost** | **Trivial** — three divisions per histogram row, in the existing serialization loop |
| **Unlocks** | #C3 Fat-Class Scatter, normalized table columns, "surprisingly fat" class detection |

#### D-2. `RetentionSummary`: missing TypeScript fields

| | |
|-|-|
| **Fields** | `top1_retained: u64`, `top10_retained: u64`, `top100_retained: u64` |
| **Rust file** | Already serialized — just add to `web/src/types.ts` |
| **Cost** | **Zero Rust changes** — only TypeScript interface update |
| **Unlocks** | Absolute-byte retention funnel chart, KPI badges |

#### D-3. `ThreadInfo`: raw JVMTI thread-state bitmask

| | |
|-|-|
| **Fields** | `thread_status_raw: i32` |
| **Rust file** | `src/report/build.rs` (~line 2200, `ThreadInfo` construction) |
| **Cost** | **Trivial** — one field forward from `ThreadProps.thread_status` |
| **Unlocks** | Fine-grained thread state breakdown (SLEEPING vs. WAITING_INDEFINITELY vs. PARKED) |

#### D-4. `BiggestCollectionRow`: split key/value types

| | |
|-|-|
| **Fields** | `key_type: Option<String>`, `value_type: Option<String>` |
| **Rust file** | `src/report/build.rs` (inside `element_type_display()`) |
| **Cost** | **Small** — values already computed, just split into separate fields |
| **Unlocks** | Filter/sort biggest collections by key or value type without string parsing |

#### D-5. `ImmDomPair`: dominator-side retained

| | |
|-|-|
| **Fields** | `dominator_retained: u64` |
| **Rust file** | `src/report/build.rs` (ImmDomPair aggregation loop) |
| **Cost** | **Small** — one extra `u64` accumulation in existing loop |
| **Unlocks** | Two-sided Sankey with dominator-side weights, normalized heatmap cells |

#### D-6. `type_ref_graph` wrapper: truncation transparency

| | |
|-|-|
| **Fields** | Wrap `Vec<TypeEdge>` in `TypeRefGraph { edges, truncated: bool, total_count: u64 }` |
| **Rust file** | `src/report/build.rs` (~line 540) |
| **Cost** | **Trivial** — struct wrapper + two fields |
| **Unlocks** | "Showing top 500 of 12,847 class-pair edges" label in the UI |

#### D-7. `LoaderRollup`: parent loader ID

| | |
|-|-|
| **Fields** | `parent_loader_id: Option<u32>` |
| **Rust file** | `src/pass2/model.rs` (LoaderRollup) + `src/report/build.rs` |
| **Cost** | **Small** — resolve parent from `java.lang.ClassLoader.parent` reference |
| **Unlocks** | #F1 ClassLoader Topology Tree |

#### D-8. `HistRow.max_dom_depth`: per-class maximum dominator chain depth

| | |
|-|-|
| **Fields** | `max_dom_depth: Option<u32>` |
| **Rust file** | `src/retained.rs` (DFS loop) |
| **Cost** | **Medium** — add `u32` scratch array indexed by `class_idx`, update during DFS |
| **Unlocks** | Direct linked-list depth measurement per class, complements `SelfReferentialClassRow` |

#### D-9. `AllocSite`: per-class breakdown

| | |
|-|-|
| **Fields** | `class_breakdown: Vec<(String, u32, u64)>` (class_name, count, retained) per site |
| **Rust file** | `src/report/build.rs` (inside `build_alloc_sites`) |
| **Cost** | **Medium** — new O(n) cross-tab of `alloc_stack_serial × class_idx` |
| **Unlocks** | "Which stack trace allocated the most of class X?" question |

---

## Implementation Roadmap

### Sprint 0 — Rust data enrichment (one PR, ~1 day)

Bundle the trivial backend additions. These unblock multiple frontend views.

| Item | Rust LOC | Unlocks |
|------|:--------:|---------| 
| D-2 (TS types for retention summary) | 0 | Retention KPI badges |
| D-1 (HistRow avg_shallow / pct) | ~8 | #C3 Fat-Class Scatter |
| D-3 (thread_status_raw) | ~3 | #E1 fine-grained breakdown |
| D-6 (type_ref_graph truncation) | ~10 | Sankey "N of M" label |
| **Total** | **~21** | |

### Week 1 — Zero new packages, zero backend (just frontend)

All items use existing libraries and existing report data.

| # | Task | LOC | Lib used | Value |
|---|------|:---:|----------|-------|
| C1 | Category Budget treemap | 15 | `FlatTreemap` | First question every user asks |
| I2 | Package Retention treemap | 20 | `ZoomableTreemap` | 20 LOC for a "By Package" toggle |
| C2 | Framework Footprint panel | 30 | `HBar` / table | Completely unvisualized data |
| D1 | Waste Waterfall treemap | 40 | `FlatTreemap` | Reclaimable memory at a glance |
| G1 | Growth Sparklines | 40 | Pure SVG `<polyline>` | Trend lines in diff table |
| A5 | Self-Referential Chain Map | 50 | `VBar` + table | 0% visualized, always-on |
| E1 | Thread State Breakdown | 50 | `FlatTreemap` | Simple grouping over ThreadInfo |
| E2 | ThreadLocal Leak Analyzer | 50 | Table + KPI | 0% visualized, high-value |
| H1 | Fix ROI Priority Ranker | 50 | Table + bars | Decision support |
| B2 | Blame Propagation bars | 60 | Pure CSS | Replaces existing renderer |
| I1 | Reference Leak Heatmap | 60 | `FlatTreemap` / `HBar` | 0% visualized, always-on |
| B3 | GC Root Retained Breakdown | 70 | `FlatTreemap` + rows | "Static vs thread vs JNI" |
| D3 | String Universe classifier | 70 | `FlatTreemap` | Makes duplicates section actionable |
| H2 | Natural-Language Triage | 80 | Template strings | Plain-English summary |
| **Total** | | **~685** | **All existing** | |

### Week 2 — Charts requiring D3 layout but no new packages

| # | Task | Lib used | LOC | Notes |
|---|------|----------|:---:|-------|
| A1 | Dominator Sunburst | `d3-hierarchy partition()` + SVG | 80 | Manual arc paths (demo proves it) |
| B1 | Retention Sankey | `d3-sankey` (installed) + SVG | 120 | Manual SVG rendering |
| A2 | Who-Holds-Whom Heatmap | Pure CSS `rgba()` | 50 | HTML table + backgrounds |
| A3 | Big Drop Waterfall | Pure SVG | 70 | Manual `<rect>` elements |
| B4 | NIO Buffer Attribution | `VBar` + `RootPathStep` | 60 | Reuses existing path renderer |
| D4 | Tiny-Collection Overhead | Table + CSS bar | 70 | Table with inline bar |
| C4 | Size Distribution Histogram | `VBar` (existing) | 60 | Two panels: objects + arrays |
| **Total** | | | **~510** | |

### Week 3 — Backend additions + remaining frontend

| # | Task | Rust Δ | Frontend LOC |
|---|------|:------:|:------------:|
| D-4 (key/value types) | 10 | 0 |
| D-5 (dominator_retained) | 8 | 0 |
| C3 | Fat-Class Scatter | 0 (D-1 done) | 70 |
| D2 | Collection Scatter | 0 | 80 |
| F2 | Duplicate Class Clash | 0 | 80 |
| **Total** | | **~18 Rust** | **~230 FE** |

### Month 2 — Larger / opt-in / niche work

| # | Task | Library | Notes |
|---|------|---------|-------|
| F1 | ClassLoader Topology | `d3-hierarchy tree()` | Needs D-7 (parent_loader_id), ~85 LOC total |
| I4 | Alloc Flame Graph | `ZoomableTreemap` (existing) | Only when `traces_present`, ~80 LOC |
| G2 | Animated Diff | CSS transitions | Nice-to-have, ~40 LOC |
| D5 | Collection Type Matrix | Pure CSS heatmap | Niche, ~60 LOC |
| D6 | Field Null-Rate Inspector | Table | Opt-in `--field-stats`, ~80 LOC |
| I3 | Unreachable Composition | `FlatTreemap` + `DomSubtreeSvg` | Secondary concern, ~90 LOC |
| G3 | Type-Edge Delta | Table + CSS overlay | Requires `--obj-graph`, ~90 LOC |

---

## Verdict Summary Table

| # | Idea | Verdict | Data ready? | New packages? | Effort |
|---|------|:-------:|:-----------:|:-------------:|:------:|
| A1 | Dominator Sunburst | 🟢 | Yes | None | 80 |
| A2 | Who-Holds-Whom Heatmap | 🟢 | Yes | None | 50 |
| A3 | Big Drop Waterfall | 🟢 | Yes | None | 70 |
| A4 | Chord Diagram | ⚪ | Yes | `d3-chord` (2.4 KB) | 80 |
| A5 | Self-Referential Chain Map | 🟢 | Yes | None | 50 |
| B1 | Retention Flow Sankey | 🟢 | Yes (opt-in) | None | 120 |
| B2 | Blame Propagation Bars | 🟢 | Yes | None | 60 |
| B3 | GC Root Retained Breakdown | 🟢 | Yes | None | 70 |
| B4 | NIO Buffer Attribution | 🟢 | Yes | None | 60 |
| C1 | Category Budget Treemap | 🟢 | Yes | None | **15** |
| C2 | Framework Footprint | 🟢 | Yes | None | 30 |
| C3 | Fat-Class Scatter | 🟢 | D-1 needed | None | 70+8 |
| C4 | Size Distribution Histogram | 🟢 | Yes | None | 60 |
| D1 | Waste Waterfall | 🟢 | Yes | None | 40 |
| D2 | Collection Scatter | 🟢 | Yes (opt-in) | None | 80 |
| D3 | String Universe Classifier | 🟢 | Yes (opt-in) | None | 70 |
| D4 | Tiny-Collection Overhead | 🟢 | Yes (opt-in) | None | 70 |
| D5 | Collection Type Matrix | 🟡 | Yes (opt-in) | None | 60 |
| D6 | Field Null-Rate Inspector | 🟡 | Yes (opt-in) | None | 80 |
| E1 | Thread State Breakdown | 🟢 | Yes | None | 50 |
| E2 | ThreadLocal Leak Analyzer | 🟢 | Yes (opt-in) | None | 50 |
| F1 | ClassLoader Topology | 🟡 | D-7 needed | None | 60+25 |
| F2 | Duplicate Class Clash | 🟡 | Yes | None | 80 |
| G1 | Growth Sparklines | 🟢 | Yes | None | 40 |
| G2 | Animated Diff Treemap | 🟡 | Yes | None | 40 |
| G3 | Type-Edge Delta | 🟡 | Yes (opt-in) | None | 90 |
| H1 | Fix ROI Priority Ranker | 🟢 | Yes | None | 50 |
| H2 | Natural-Language Triage | 🟢 | Yes | None | 80 |
| I1 | Reference Leak Heatmap | 🟢 | Yes | None | 60 |
| I2 | Package Retention Treemap | 🟢 | Yes | None | **20** |
| I3 | Unreachable Composition | 🟡 | Yes | None | 90 |
| I4 | Alloc Flame Graph | 🟡 | Rare data | None | 80 |

**Summary:** 22 🟢 Implement, 9 🟡 Consider, 1 ⚪ Defer. **Zero** ideas require new npm packages.

**Total new frontend LOC (all 🟢 ideas):** ~1,295  
**Total new Rust LOC (Sprint 0 data enrichment):** ~21  
**New npm dependencies:** 0  
**Bundle size increase:** 0 KB

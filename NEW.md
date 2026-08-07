# hprof-analyzer — Feature Research & Gap Analysis

> Written 2026-08-02. Based on: full codebase audit, Eclipse MAT docs, JProfiler features,
> YourKit feature list, HeapHero, JXRay, IntelliJ IDEA memory view, async-profiler,
> existing `docs/BIG_PLAN.md`.

---

## 1. What Already Exists (Audit-Verified)

This section corrects assumptions. Many things that seem missing are already built.

| Feature | Status | Notes |
|---------|--------|-------|
| Dominator tree explorer (3 modes) | ✅ Built | flat / grouped / expanded views |
| Outbound ref filter + pagination | ✅ Built | `refFilter`, 50/page |
| Domtree child filter | ✅ Built | `domFilter` on flat view; `expandFilter` on expanded view |
| Peer sibling navigation `[ / ]` | ✅ Built | keyboard, same-class sorted by retained |
| Jump box (by dense index) | ✅ Built | in root list + node detail |
| Breadcrumb navigation | ✅ Built | backward only, max 10 entries |
| Sankey ("Who Holds This Class?") | ✅ Built | `WhoHoldsSankey`, two-sided pivot |
| d3-sankey in bundle | ✅ Built | package.json, used for 2 sankeys |
| ImmDomPair backend data | ✅ Built | in `report/build.rs`, serialized, wired to sankey |
| Executive summary / triage card | ✅ Built | `ExecSummaryCard` + `OomTriage` |
| Thread by retained heap table | ✅ Built | `ThreadsByRetainedTable` |
| ThreadLocal leak analyzer | ✅ Built | `ThreadLocalAnalysisTable` per-class |
| GC roots by type + top classes | ✅ Built | `gc_roots_retained_by_type.top_classes` |
| find_instances (WASM live search) | ✅ Built | root list only; class substring search |
| Below-threshold WASM panel | ✅ Built | refs + GC path for out-of-graph nodes |
| MergedPathSankey for group suspects | ✅ Built | retention path sankey in leak suspects |
| Object address (WASM) | ✅ Built | `get_object_address`, shown in Details panel |
| OQL in browser + completion | ✅ Built | WASM query panel + complete_query |
| Path to GC root (shortest BFS) | ✅ Built | `WasmGcPathPanel` + dominator chain |
| Shared badge (⟲) for cross-dominators | ✅ Built | idom check on each outbound edge |
| Inbound refs (static + WASM) | ✅ Built | `WasmInboundPanel` + static `inbound_edges` |
| DomSubtreeSvg pre-built trees | ✅ Built | for top-N roots in leak suspects |

**What is NOT built** (confirmed missing from code):
- Primitive/String field values in object explorer
- Forward navigation button
- find_instances in main node view (root list only)
- Cycle detection / visited-node highlighting
- Field values in WASM outbound_refs response

---

## 2. Competitor Feature Matrix

### Eclipse MAT

The dominant free tool — industry reference point.

**Unique to MAT, not in hprof-analyzer:**
- **All retention paths to GC roots** (not just shortest): given an object, shows *every* unique reference chain to a GC root, grouped by type. Our BFS gives the shortest single path. MAT's "merge shortest paths" shows the full forest, which reveals when an object is retained by *multiple* independent chains.
- **Reference type filtering on paths**: exclude soft/weak/phantom references from path queries. Useful when investigating whether an object would be collectible under memory pressure.
- **Object attribute values in explorer**: every object viewer shows decoded field values — `String value = "hello"`, `int size = 47823`. Zero-click; available for any selected object. Our explorer shows only class + size.
- **OQL results linked to object explorer**: click any row in OQL results to open that object in the reference explorer. In hprof-analyzer, OQL results are a flat table with no link into the graph explorer.
- **Retained heap per OQL result column**: `SELECT @retainedHeapSize` in OQL; results show retained automatically alongside class. We surface retained in WASM OQL results but it requires explicit column selection.
- **Leak suspects with merge paths**: "Run Leak Suspects Report" finds big groups, then "Merge Shortest Paths to GC Roots" produces a class-prefix tree of all retention chains. Our leak suspects show merged paths as a sankey, but MAT's tabular tree is more scannable when paths fork heavily.
- **"Group by value"** in histogram: show only unique String values, count duplicates inline.
- **Regex support** in class filter.

**MAT gaps vs hprof-analyzer:**
- No web — must install Eclipse plugin or standalone app
- No shareable HTML report
- Must load entire dump into RAM (64 GB heap = 64 GB RAM required)
- No WASM / browser analysis
- No diff reports
- 2005-era UX — easy to lose navigation context after a few clicks
- No collection fill-ratio analysis

---

### JProfiler Heap Walker

Commercial (~€500/seat). Best-in-class heap walker UX.

**Unique to JProfiler, not in hprof-analyzer:**
- **"Merged Dominating References"** (their most-cited feature): pick any class, JProfiler shows all reference chains from GC roots to instances of that class, merged by field name, sortable by instance count or retained size. Differs from our sankey in that each node is collapsible and shows per-field counts, not just class-level aggregates.
- **Reference graph with visited-node highlighting**: when navigating, objects already in your current path are visually marked, preventing disorientation in circular-referencing structures.
- **Heap comparison**: two snapshots → diff showing which classes gained/lost instances. Per-instance allocation traces when `-agentlib` is active.
- **Field values in object view**: shows decoded primitive + String field values for the selected instance.
- **Allocation site linking**: when `-agentlib` is active at dump time, shows which call stack allocated each live object. "This HashMap was allocated at RequestHandler.handle line 142."
- **Incoming reference count in histogram**: each class row shows not just instance count but *how many other objects reference this class*, making it easy to spot classes that are referenced far more than expected.

**JProfiler gaps vs hprof-analyzer:**
- Cannot share results — no HTML export
- No browser-based analysis
- Expensive; requires license per developer
- Must load full heap into RAM

---

### YourKit Java Profiler

Commercial, ~same price range as JProfiler. Different UX emphasis.

**Unique to YourKit, not in hprof-analyzer:**
- **40+ automatic inspections**: runs at snapshot load time, surfaces cards for: leaked web apps, unclosed SQL statements/streams, inefficient collections, non-null-key ThreadLocals, duplicate objects. Results are shown as concrete "you have 47 unclosed PreparedStatements" cards — not queries to run, but already-computed findings.
- **Object graph traversal "in any direction"**: YourKit's explicit claim is that you can follow references in both directions (inbound + outbound) from any object, not just the ones in a pre-captured set. Similar to our WASM mode but available for every object, not just below-threshold ones.
- **"Estimate fix impact"**: select any reference edge and YourKit estimates how much memory would be freed if that reference were removed. Computed from retained size of the referenced subgraph.
- **Smart collection display**: HashMap shows as key→value pairs, ArrayList shows as indexed list — not as the raw `.table` backing array. Significantly reduces navigation depth for common patterns.
- **Retained size calculation on demand**: not computed at load; triggered per-object. Faster initial load, but means retained is N/A until explicitly calculated.
- **Snapshot timeline**: multiple snapshots on a GC activity timeline, click any snapshot to open it. Good for correlating heap state with GC events.

**YourKit gaps vs hprof-analyzer:**
- No shareable HTML
- No browser-based analysis
- Retained sizes are lazy (not pre-computed), so "sort by retained" requires waiting

---

### HeapHero (web SaaS)

Most direct competitor to our shareable HTML report idea.

**What HeapHero has:**
- Upload .hprof → get shareable URL with analysis
- "At a Glance" card with ML-powered leak classification
- Dominated tree + histogram + outgoing/incoming references
- OQL query interface
- 30–70% memory waste detection (duplicate strings, empty collections, boxed numbers)
- Detects problematic source code lines
- Shareable link for team collaboration

**HeapHero gaps vs hprof-analyzer:**
- Requires uploading sensitive heap data to their servers (privacy issue)
- No WASM / browser-native analysis
- No offline analysis
- No diff reports
- No OQL in browser (their OQL is server-side)
- Slower (server round-trip per analysis)

---

### JXRay

Open-source, self-contained HTML report. Architecturally closest to hprof-analyzer.

**What JXRay has:**
- Self-contained HTML output, offline, shareable
- Proactive anti-pattern detection: duplicate objects, underutilized collections, boxed numbers
- Handles up to 512 GB, headless, CI-friendly
- Concrete code-change recommendations

**JXRay gaps vs hprof-analyzer:**
- No interactive object graph navigator
- No dominator tree explorer
- No OQL
- No WASM / browser analysis mode
- Java (slower than Rust for large heaps)
- Static-only report — no drill-down beyond what was pre-rendered

---

### IntelliJ IDEA Memory Snapshot Viewer

Built into IntelliJ IDEA (free tier includes basic heap analysis).

**What IntelliJ has:**
- Class histogram with retained + shallow sizes
- "Biggest Objects" dominator tree roots
- "Merged Paths" — per-class retention path grouping
- "GC Roots" view per class
- "Shortest Paths" to GC roots per instance
- "Incoming References" per instance
- "Retained Objects" sunburst diagram
- "Dominator Tree" subtree view
- Speed search (type to filter classes instantly)
- Navigate to source code (F4) — jumps to the class definition in the editor

**IntelliJ gaps vs hprof-analyzer:**
- No shareable report
- No OQL
- No WASM / browser mode
- No collection waste analysis
- No diff reports
- Source navigation only works if you have the source loaded

---

### async-profiler

Not a heap dump analyzer — allocation profiler. But increasingly used alongside heap dumps.

**What it adds that heap analysis tools don't:**
- Allocation flamegraph: shows which *call stacks* are allocating objects, sampled with very low overhead (< 1% CPU)
- Works in production — no `-agentlib:hprof=heap=all` required
- Shows the *allocation site* of surviving objects when combined with heap snapshots (async-profiler allocation trace + hprof heap dump at same moment)

**Gap for hprof-analyzer:** our allocation flamegraph (V10) only works when the HPROF itself contains allocation traces, which is rare. Integration with async-profiler allocation data would be more broadly useful but requires a different input format.

---

## 3. Real Gaps — What's Missing and Worth Building

Based on the audit (what's actually missing) + competitor research (what users value most), ordered by impact-to-effort.

---

### Tier 1 — Frontend only, no WASM rebuild, high impact

#### A. `find_instances` in main node view  
**Gap:** "Top N instances of this class by retained" is only shown on the below-threshold dead-end page. In the main node view for any captured object, there's no quick way to see "are there other instances of this class, and how big are they?"  
**Why it matters:** When debugging a leak, the standard workflow is: find big object → is it unique or is there a whole population of them? Right now you have to go back to root list and use live instance search.  
**How to implement:** After loading `currentNode.display_class`, call `wasm.find_instances(cls, 10)` and show a compact "N instances of ClassName, top by retained" row with clickable links in the Object Details panel. 3-4 lines of JSX, one WASM call.  
**Effort:** S (< 1 hour)  
**Competitor:** JProfiler shows incoming reference count in histogram; YourKit lists all instances per class.

---

#### B. Forward navigation  
**Gap:** Breadcrumb goes backward but there's no forward button. After pressing `← back`, the user loses their forward history.  
**Why it matters:** This is the single most common navigation complaint in every heap analysis tool (MAT, VisualVM all lack it). YourKit explicitly advertises it. Standard browser UX.  
**How to implement:** Add `forwardStack` state (array of the same shape as breadcrumb entries). On `navigate()`, push current to forward stack if we're moving to a new node. On back, pop from breadcrumb and push to forward stack. On forward, pop from forward stack and push to breadcrumb. Clear forward stack on any "new" navigation (clicking a link, not back/forward).  
**Effort:** S (1–2 hours)  
**Competitor:** YourKit has forward navigation.

---

#### C. Outbound ref filter is hidden too aggressively  
**Gap:** The `refFilter` input only appears when a node has > 5 grouped edges. For nodes with exactly 3–5 edges that include a mix of field names, there's no way to filter.  
**Why it matters:** Annoyance for small objects with mixed fields. Minor but low-effort fix.  
**How to implement:** Lower threshold to 2, or show filter always when > 0 edges.  
**Effort:** XS (10 minutes)

---

#### D. Domtree "by class" view filter  
**Gap:** `domFilter` filters the flat immediate-children view but NOT the "by class" grouped view. If a node has 50 child classes and you want to find "byte[]" you have to switch to flat view.  
**How to implement:** Apply `domFilter` to the grouped view's rows the same way it's applied to flat (line 6745-6748 pattern).  
**Effort:** XS (15 minutes)

---

#### E. OQL result rows linked to object explorer  
**Gap:** WASM OQL results are a flat table. When a query returns instances (e.g. `SELECT * FROM java.util.HashMap h`), there's no way to click a row and navigate to that object in the explorer. In Eclipse MAT this is the primary post-OQL workflow.  
**Why it matters:** OQL finds the object; the explorer reveals why it's alive. These two features are currently disconnected.  
**How to implement:** The WASM `query()` JSON response includes a `dense_idx` for object-type results (check: does it?). If yes, add an `ExploreBtn` / `→` button per result row. If `dense_idx` is not in the current response, add it to the WASM `query()` output (requires WASM rebuild).  
**Blocker:** Need to verify if `dense_idx` is in OQL result rows. Check `src/run_oql.rs` and `crates/hprof-wasm/src/lib.rs` query() output format.  
**Effort:** S–M depending on whether dense_idx is already in results  
**Competitor:** Eclipse MAT has this; it's the standard post-OQL workflow.

---

#### F. Cycle / revisit detection badge  
**Gap:** When following references leads back to an object already in the breadcrumb trail (e.g. a doubly-linked list, tree with parent pointers), there's no visual signal. The user doesn't know they've looped.  
**Why it matters:** Reference graphs can have back-edges (A → B → A is possible in Java: parent pointers, observer pattern, etc.). Currently invisible.  
**How to implement:** Maintain a `Set<number>` of dense indices in the current breadcrumb. When rendering outbound/inbound ref rows, check if `child_idx` is in the set. If so, add a 🔄 or "↩ already in path" badge.  
**Effort:** S (1 hour)  
**Competitor:** JProfiler highlights already-visited nodes.

---

### Tier 2 — Requires new Rust/WASM code, high value

#### G. Primitive/String field values in WASM object explorer  
**Gap:** Every MAT/JProfiler/YourKit user expects to see `String value = "hello"` or `int count = 47823` when clicking on an object. Currently we show only class + shallow + retained — no field values at all.  
**Why it matters:** The most common next question after "what class is this?" is "what does it contain?" Without field values, users can't tell which HashMap instance is the leaking cache vs. a normal one.  
**How to implement:**  
  1. Add `get_field_values(dense_idx: u32) -> String` to `HprofSession` in `crates/hprof-wasm/src/lib.rs`
  2. Uses existing `Pass1` class map + the stored compressed hprof bytes to decode the object's instance fields
  3. Returns JSON: `{"ok":true,"fields":[{"name":"size","type":"int","value":47823},{"name":"threshold","type":"int","value":32768},{"name":"table","type":"ref","value":"java.util.HashMap$Entry[]","dense_idx":1234}]}`
  4. UI: show field table in Object Details panel when available, with navigate buttons for reference-type fields
**Effort:** M (½ day Rust + ½ day UI). Requires WASM rebuild.  
**Competitor:** Available in MAT, JProfiler, YourKit, IntelliJ. Conspicuously absent in hprof-analyzer.  
**Note:** The compressed hprof bytes are stored in `HprofSession` already (for OQL). The field-decode infrastructure exists in `pass2/`. The main new code is looking up the object's offset in the compressed data and decoding instance fields.

---

#### H. All-paths to GC root (not just shortest)  
**Gap:** `gc_root_path` returns the single shortest BFS path. MAT shows all paths, which reveals when an object is multiply retained.  
**Why it matters:** When debugging why an object can't be collected, the answer is often "it's held by 3 independent chains, you need to fix all 3." Shortest-path only shows one.  
**How to implement:**  
  - New WASM method `all_gc_root_paths(dense_idx, max_paths)` that runs multi-source BFS and returns up to N distinct paths
  - Each path is a chain from a GC root to the target object
  - Cap at 10 paths to keep output size reasonable
  - UI: show each path collapsibly below the existing "shortest path" section
**Effort:** M (1 day Rust + 2 hours UI). Requires WASM rebuild.  
**Competitor:** Eclipse MAT "Path to GC Roots" is this exact feature.

---

#### I. Reference type filtering on GC root path  
**Gap:** BFS doesn't distinguish strong vs. soft/weak/phantom references. A soft-reference holder appears in GC root path as if it's a strong retainer.  
**Why it matters:** Investigating "why isn't the GC collecting this?" often requires knowing whether the retention is via strong or soft references. An object held only via soft refs will be collected under memory pressure; an object held via strong refs won't.  
**How to implement:**  
  - `gc_root_path` BFS must tag each edge with its reference strength (strong/soft/weak/phantom)
  - Requires `pass2` to record reference type on edges — currently only field names are stored
  - Add a UI toggle: "Show only strong-reference paths"
**Effort:** M-L (requires changes deep in pass2 edge capture)  
**Competitor:** Eclipse MAT has this toggle.  
**Priority:** Lower than G and H — less commonly needed.

---

#### J. Smart collection display  
**Gap:** A `HashMap` node shows outbound refs to its raw `table Entry[]` backing array, which then has 16,384 `Entry` nodes. To find any key-value pair you navigate through 3 layers of implementation detail.  
**Why it matters:** YourKit's most notable UX win: "HashMap shows as key→value pairs." Reduces 3-click navigation to 1-click for the most common Java data structures.  
**How to implement:**  
  - New WASM method `get_collection_entries(dense_idx, limit)` that knows about `HashMap`, `ArrayList`, `HashSet`, `LinkedList` internals and returns key/value or element pairs directly
  - Requires framework knowledge of JDK collection internals — the field layout of `HashMap.table`, `ArrayList.elementData`, etc.
  - Returns: `{"ok":true,"type":"map","entries":[{"key_idx":N,"key_class":"...","val_idx":M,"val_class":"..."}]}`
  - UI: new "Collection entries" sub-panel when the WASM reports the node is a known collection type
**Effort:** L (2–3 days Rust — needs field-decode for each collection type + handling JDK version differences in internal layout)  
**Competitor:** YourKit has this; IntelliJ partially has it.

---

### Tier 3 — New sections / backend work, distinct value

#### K. "Peer" instances panel at top of node view  
**Gap:** `find_instances` in the root list finds instances of any class. But once you're looking at node X, you can't easily ask "what are the 10 other biggest instances of this class?"  
**How to implement:** When `wasmExploration` is live, after loading a node call `find_instances(currentNode.display_class, 10)`. Show as a compact table: dense_idx | retained | navigate button. Placed near the "Object Details" panel.  
**Effort:** XS–S (< 1 hour: 1 WASM call + ~10 lines JSX)  
**Note:** Different from `find_instances` in root list (which is a global search) — this is scoped to the current class.

---

#### L. Two-dump retained diff  
**Gap:** Diff reports (`hprof-analyzer diff a.hprof b.hprof`) show class-level shallow heap delta but not retained heap delta. Retained delta is far more useful for leak diagnosis.  
**Why it matters:** Classes that grew in *shallow* heap may just be allocating more. Classes that grew in *retained* heap are likely holding more objects alive — the direct leak signal.  
**How to implement:**  
  - Add `retained_a`, `retained_b`, `retained_delta` to `DiffHistRow` in `report/model.rs`
  - Populate in `diff_reports.rs` by running retained computation on both dumps
  - Add sortable "Δ Retained" column to diff table in UI
**Effort:** M (1 day backend + 2 hours UI)  
**Competitor:** JProfiler, dotMemory heap comparison.  
**Planned as:** V12 in BIG_PLAN.md

---

#### M. Type-level reference graph (sankey + table)  
**Gap:** Class-to-class reference weights. Which class references which class most, by retained weight? Answers "what is the retention topology of this heap?" before reading any individual object.  
**How to implement:**  
  - New aggregation pass in `report/build.rs`: scan `fwd_targets`, group by `(src_class_idx, dst_class_idx)`, sum edge counts + retained weight
  - Emit as `type_edges: Vec<TypeEdge>` in report JSON (top 500 by retained weight)
  - UI: d3-sankey view (top 15 edges) + sortable table (all 500)
  - d3-sankey is already bundled; no new dependency
**Effort:** M (½ day backend + ½ day UI)  
**Planned as:** V13 in BIG_PLAN.md  
**Note:** This is unique — no competitor has this as a visual in a shareable report.

---

#### N. Framework auto-detection  
**Gap:** HeapHero and JProfiler detect Hibernate sessions, HikariCP connection pools, Spring contexts at analysis time and surface dedicated cards.  
**Why it matters:** These are the most common sources of production leaks. "12 unclosed Hibernate sessions retaining 890 MB" requires zero expertise to diagnose.  
**How to implement:**  
  - New `pass2/framework_scan.rs`: check histogram for sentinel classes (`org.hibernate.internal.SessionImpl`, `com.zaxxer.hikari.pool.HikariPool`, etc.)
  - Decode relevant fields per detected framework
  - Emit `framework_analysis: Vec<FrameworkCard>` in report
  - UI: show cards when present
**Effort:** M-L (1–2 days backend per framework; start with Hibernate + HikariCP)  
**Planned as:** V19 in BIG_PLAN.md

---

#### O. Lambda class grouper in histogram  
**Gap:** `$$Lambda$4821/0x00007f2b3c` class names in histogram are opaque. 400 distinct lambda names each with 5 MB look harmless individually but represent 2 GB of closure leak when grouped by enclosing class.  
**How to implement:**  
  - Client-side: regex transform on histogram class names before rendering
  - Group `$$Lambda$NNN/...` → enclosing class prefix, with `[λ ×count]` badge
  - Toggle to show raw names
  - No backend changes
**Effort:** S (2–3 hours)  
**Planned as:** V8 in BIG_PLAN.md  
**Competitor:** JProfiler partially does this.

---

## 4. Quick-win Implementation Details

### A + K combined: "Same-class instances" panel in node view

```tsx
// In the main node view (below Object Details), when WASM exploration is live:
{nodeId !== null && (() => {
  const wasm = (window as any).__wasmExploration;
  if (!wasm?.find_instances || !currentNode) return null;
  // call once per nodeId change — use a useEffect similar to wasmBelowInfo
  // show top 8 instances sorted by retained, excluding current node
  const peers = wasmPeerInstances?.filter(p => p.dense_idx !== nodeId).slice(0, 8);
  if (!peers?.length) return null;
  return (
    <div> ... </div>
  );
})()}
```

State needed: `wasmPeerInstances` populated by a `useEffect` on `[nodeId, currentNode?.display_class]`.

### B: Forward navigation

```tsx
const [forwardStack, setForwardStack] = React.useState<BreadcrumbEntry[]>([]);

// in navigate():
setForwardStack([]);  // clear on any new navigation

// in goBack():
const last = breadcrumb[breadcrumb.length - 1];
setForwardStack(prev => [...prev, {nodeId: current, ...}]);
setBreadcrumb(prev => prev.slice(0, -1));

// forward button:
const goForward = () => {
  if (!forwardStack.length) return;
  const next = forwardStack[forwardStack.length - 1];
  setForwardStack(prev => prev.slice(0, -1));
  // push current to breadcrumb, navigate to next
};
```

Button: `{forwardStack.length > 0 && <button onClick={goForward}>→ forward</button>}`

### E: OQL dense_idx check

Need to audit `src/run_oql.rs` and the WASM `query()` output. If object-type SELECT results include `@objectId` or `dense_idx`, wire `ExploreBtn` per row. If not, the simplest path is to check if each row value is an object type and add `get_object_address`-style lookup to map back to a dense index.

---

## 5. Priority Order

Recommended sequence:

1. **K** — same-class peer panel (XS, high daily-use value)
2. **B** — forward navigation (S, very visible UX gap)
3. **C/D** — filter threshold fixes (XS, minor annoyance)
4. **F** — cycle badge (S, enables debugging circular structures)
5. **E** — OQL→explorer link (S–M, closes biggest workflow gap vs. MAT)
6. **O** — lambda grouper (S, makes lambda-heavy heaps readable)
7. **G** — field values WASM (M, WASM rebuild required, but very high value)
8. **L** — two-dump retained diff (M, backend work, targeted at repeat users)
9. **M** — type-level reference graph (M, unique feature no competitor has)
10. **H** — all-paths to GC root (M, WASM rebuild, power-user feature)
11. **N** — framework auto-detection (L, highest value if Hibernate/HikariCP present)
12. **J** — smart collection display (L, highest UX payoff but complex internals)

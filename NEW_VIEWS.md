# New Views & Additions — hprof-analyzer

Novel ideas for accelerating root-cause analysis of Java heap dumps.
Each entry has a description, mockup, data source, and implementation notes.

---

## 1. Retention Flamegraph (new section)

**What:** A flame-chart where each row is a dominator-tree depth level.
Width of each box is retained bytes. Hover to see class name + bytes.
Click to drill into that class's subtree. Similar to a CPU flamegraph but
for memory — the widest boxes at the top are your biggest leaks.

**Why it helps:** Instantly shows which class "owns" the most heap at each
depth, without scrolling through tables. The visual shape reveals clusters
of deep retention (long thin flames = linked lists; fat top = single big holder).

```
 Retained Flamegraph                                  [⬇ PNG] [Fit]
 ┌──────────────────────────────────────────────────────────────────┐
 │ depth 0 (GC roots)                                               │
 │ ████████████████████████████████████████████████████████████████ │  11.7 MB
 ├──────────────────────────────────────────────────────────────────┤
 │ depth 1                                                          │
 │ ████████████████████████ InTxnImpl     │ ████████ LazyVals$ │ ██ │
 │ 2.7 MB                                 │ 2.5 MB             │    │
 ├──────────────────────────────────────────────────────────────────┤
 │ depth 2                                                          │
 │ ████████████████ Object[]  │ ████████ HashMap │ █ String[] │ ██  │
 ├──────────────────────────────────────────────────────────────────┤
 │ depth 3                                                          │
 │ ███████████ HashMap$Node   │ ████ String  │ █ byte[]       │ ██  │
 ├──────────────────────────────────────────────────────────────────┤
 │ depth 4                                                          │
 │ ████ ZipFile$Source │ ████ SoftReference │ ██ JarFile │ ██ char[]│
 └──────────────────────────────────────────────────────────────────┘
  Each box: click to select class  · hover for exact bytes & %
  Color = class package (same hue = same package)
```

**Data:** `idom_pairs` + `biggest_classes` (retained by depth level).
Can approximate depth from BFS over `idom_pairs` graph.
**Placement:** New section between Dominator Analysis and Threads.

---

## 2. Retention Heatmap Matrix (new section)

**What:** A matrix where rows = top dominator classes, columns = top dominated
classes. Each cell is colored by retained bytes flowing from row→column.
Essentially a visual version of the `idom_pairs` table.

**Why it helps:** Reveals cross-class retention patterns in one glance — e.g.,
"HashMap$Node dominates String in 3 different paths" shows up as a hot cell.
Identifies unexpected class relationships (e.g. ZipFile holding InTxnImpl).

```
 Retention Heatmap  (top 15×15 by retained flow)      [Show as KB]
 ┌────────────────┬──────────────────────────────────────────────────────────┐
 │  dominator↓    │ Object[] HashMap HM$Node  String  byte[]  char[] ZipFile │
 │ dominated→     │                                                           │
 ├────────────────┼──────────────────────────────────────────────────────────┤
 │ InTxnImpl      │  ████░░░  ███████  ████    ██████   ████   ░░░   ░░░░░  │
 │ LazyVals$      │  ████████ ██░░░░░  ███░    ███░░░   ██░░   ░░░   ░░░░░  │
 │ Class          │  ░░░░░░░  ░░░░░░░  ███░    ████████ ██░░   █████ ░░░░░  │
 │ ZipFile$Source │  ░░░░░░░  ░░░░░░░  ░░░░    ░░░░░░░  ██████ ░░░░ ████████│
 │ SoftReference  │  ░░░░░░░  ░░░░░░░  ░░░░    ░░░░░░░  ░░░░░  ░░░░ ░░░░░  │
 └────────────────┴──────────────────────────────────────────────────────────┘
  ████ = high retained  ░░░░ = low/none   Click cell → filter graph to pair
```

**Data:** `idom_pairs` table directly.
**Placement:** Inside Dominator Analysis section, tab alongside graph.

---

## 3. Class Relationship Explorer ("Who holds what" diagram)

**What:** Pick any class. Shows a two-ring diagram:
- Inner ring: the selected class
- Middle ring: all classes that DOMINATE it (what holds it in memory)
- Outer ring: all classes it DOMINATES (what it retains)
Size of each arc = retained bytes. Clicking an arc selects that class and
recenters the diagram.

**Why it helps:** Answers "what is holding X in memory and what does X hold?"
in one visual — the key question when investigating a suspect.

```
 ┌─────────────────────────────────────────────────────────────────┐
 │  Class:  [ InTxnImpl              ▼ ]   [← Back]  [Inspect →]  │
 │                                                                  │
 │                ╭──────────────────────────╮                      │
 │    InTxnImpl   │      HELD BY             │ Object[] 2.1MB      │
 │    2.7 MB      │   (dominators)           │ Class    1.8MB      │
 │                │ ╭─────────────╮          │ HashMap  0.9MB      │
 │                │ │  ▓▓▓▓▓▓▓▓▓ │          │                      │
 │                │ │ InTxnImpl  │          │ RETAINS              │
 │                │ │  2.7 MB   │          │ (dominated)          │
 │                │ ╰─────────────╯          │ String   1.2MB      │
 │                │                          │ byte[]   0.7MB      │
 │                ╰──────────────────────────╯ char[]   0.4MB      │
 │                                                                  │
 │  ↑ Ancestors (3)  ↓ Dominated (8 classes · 2.7 MB total)       │
 └─────────────────────────────────────────────────────────────────┘
```

**Data:** `idom_pairs`.
**Placement:** Replaces/augments the "Who Holds This Class? Navigator" section,
or as a new tab inside Dominator Analysis.

---

## 4. Object Graph: Path Between Two Classes (new feature in OGE)

**What:** In the Object Graph Explorer, add a "Find path" mode:
select a source class and target class, and the UI runs BFS over
`idom_pairs`/`obj_graph_flat` to find the shortest dominator chain
between them, then highlights that path in the graph.

**Why it helps:** When you see "Class A is leaking 5 MB" and you know
"it comes from Class B", the current UI requires manual clicking to find
the chain. Path-finding automates this.

```
 ┌─────────────────────────────────────────────────────────────────────┐
 │  Find Retention Path                                                 │
 │                                                                      │
 │  From: [ InTxnImpl                    ▼ ]                           │
 │    To: [ ZipFile$Source               ▼ ]                           │
 │                                           [Find Path]               │
 │  ────────────────────────────────────────────────────────────────   │
 │  Shortest path (4 hops):                                            │
 │                                                                      │
 │   InTxnImpl ──[txnContext]──▶ HashMap ──[table]──▶ HashMap$Node     │
 │       2.7 MB                    1.5 MB               1.6 MB          │
 │        ↓                                                             │
 │   HashMap$Node ──[value]──▶ ZipFile$Source                          │
 │       1.6 MB                    1.3 MB                               │
 │                                                                      │
 │   [Jump to path in graph]   [Copy as text]                          │
 └─────────────────────────────────────────────────────────────────────┘
```

**Data:** `idom_pairs` for class-level BFS; `obj_graph_flat` for
instance-level path.
**Placement:** New toolbar button in DomGraphView and ObjectGraphExplorer.

---

## 5. Heap Timeline Sparklines in Class Histogram (enhancement to System Overview)

**What:** In the biggest-classes table, each row gets a tiny sparkline bar
showing retained-size ranking change vs. known reference points.
More importantly: add a "suspicious growth rate" flag column that marks
classes whose retained share is disproportionately large relative to
instance count (potential interning / caching accumulation pattern).

**Why it helps:** Raw retained bytes don't tell you if a class is "normal"
or "suspicious". A class with 94 instances holding 2.7 MB has a very high
bytes-per-instance ratio that often signals a cache or registry leak.

```
 Biggest Classes by Retained                    [Show KB] [⎘ TSV]
 ┌──┬───────────────────────────────┬────────┬────────┬─────────┬─────────┐
 │ #│ Class                         │Objects │Shallow │Retained │ B/inst  │
 ├──┼───────────────────────────────┼────────┼────────┼─────────┼─────────┤
 │ 1│ InTxnImpl              ⚠ high │    94  │ 13 KB  │ 2.7 MB  │  29 KB ↑│
 │ 2│ LazyVals$              ⚠ high │     1  │  32 B  │ 2.5 MB  │ 2.5 MB ↑│
 │ 3│ Class                         │ 2,793  │ 33 KB  │ 1.5 MB  │   550 B │
 │ 4│ Object[]                      │ 5,829  │  2 MB  │ 1.1 MB  │   193 B │
 │ 5│ ZipFile$Source         ⚠ high │    19  │  1 KB  │ 1.3 MB  │  69 KB ↑│
 └──┴───────────────────────────────┴────────┴────────┴─────────┴─────────┘
  ⚠ high = retained/instance >10× median for that instance count bucket
```

**Data:** `biggest_classes` + simple ratio computation.
**Placement:** Inline addition to the existing biggest-classes table.

---

## 6. GC Root Heatmap (new section / enhancement to System Overview)

**What:** A compact matrix of GC root types (rows) vs top retained classes
(columns). Each cell is a dot sized by retained bytes. Instantly shows
"Thread locals hold InTxnImpl" or "Static fields hold ZipFile$Source".

**Why it helps:** The existing GC root section just shows counts. This
shows WHICH classes are retained by WHICH root type — the direct answer
to "where is this leak anchored".

```
 GC Root Retention Matrix                                  [Show KB]
 ┌──────────────────────┬────────────────────────────────────────────────┐
 │ Root type            │  InTxn  LazyVals  ZipFile  Class  HashMap  Str │
 ├──────────────────────┼────────────────────────────────────────────────┤
 │ Thread (local var)   │  ●●●●●   ○○○○○○   ○○○○○   ○○○○○   ●●○○○  ○○○ │
 │ Static field         │  ●●○○○   ●●●●●●   ●●○○○   ●●●○○   ●●○○○  ○○○ │
 │ JNI global ref       │  ○○○○○   ○○○○○○   ●●●●●   ○○○○○   ○○○○○  ○○○ │
 │ System class         │  ○○○○○   ○○○○○○   ○○○○○   ●●●●●   ○○○○○  ●○○ │
 └──────────────────────┴────────────────────────────────────────────────┘
  ●●●●● = high retention   ○○○○○ = none   Click cell → filter Object Graph
  Hover = exact bytes · class pair
```

**Data:** `overview.gc_root_retained` (already has per-root-type class breakdown).
**Placement:** Inside System Overview, after GC root type bar chart.

---

## 7. Dominator Graph: Pinned "Blame View" (new focus mode in DomGraphView)

**What:** A new focus mode button "📌 Blame" that, when clicked, locks the
graph into a top-down tree showing only the highest-retained-bytes path from
every GC root down to the deepest node. Think of it as "critical path" of
retention — the nodes you must free to reclaim the most memory. Non-critical
nodes collapse to a "N others" group.

**Why it helps:** In a graph with 50 nodes, you can't tell which path is
"the" retention path. Blame mode reduces it to 5–8 nodes along the
dominant chain.

```
 [Force] [Tree] | [All] [▲ Retained by] [▼ Retains] [📌 Blame] | ...

 📌 Blame view — critical retention path (top 94.1% of heap)

                 ┌─────────────────┐
          ┌──────│  (GC roots)     │──────┐
          │      │   11.7 MB 100%  │      │
          │      └─────────────────┘      │
          ▼                               ▼
  ┌──────────────┐                ┌──────────────┐
  │  InTxnImpl   │                │  LazyVals$   │
  │  2.7MB 22.9% │                │  2.5MB 21.5% │
  └──────┬───────┘                └──────┬───────┘
         │                               │
         ▼                               ▼
  ┌──────────────┐                ┌──────────────┐
  │  Object[]    │                │  Object[]    │
  │  1.1MB  9.5% │                │   940KB  8%  │
  └──────────────┘                └──────────────┘
         └──── [+7 collapsed: 450KB] ───┘
```

**Data:** `idom_pairs` — walk the max-retained-child path from each root.
**Placement:** New toggle button in DomGraphView toolbar.

---

## 8. Instance Scatter Plot (new viz in Inspector / Top Consumers)

**What:** For a selected class, plot each instance as a dot on a 2D chart:
X = shallow size, Y = retained size. Color = GC root distance (depth).
Outlier instances (top-right quadrant) are your biggest individual memory
holders. Click a dot to open that instance in the Inspector.

**Why it helps:** "94 InTxnImpl instances hold 2.7 MB total" — but is it
distributed evenly, or are 2-3 instances holding most of it? The scatter
plot answers this in 1 second.

```
 InTxnImpl — Instance Scatter (94 objects)        [Open in Inspector →]
 retained
    ▲
 2.5MB │                                              ●  (outlier!)
       │
 500KB │                          ●●  ●●●
       │                    ●●●●●●●●●●●●●●
 100KB │              ●●●●●●●●●●●●●●●●●●●●●
       │         ●●●●●●●●●●●●●●●●●●●
  10KB │   ●●●●●●●●●●●●●
       └──────────────────────────────────────────→  shallow
          80B     200B   400B     800B   1.6KB
  Color: ■ depth 1  ■ depth 2  ■ depth 3+
  Hover = object address · retained · held-via field
  Click = open in Inspector
```

**Data:** Requires iterating `obj_graph_flat.nodes` filtered by class.
**Placement:** New "Scatter" tab in the Inspector class view, or button
in biggest-classes table row.

---

## 9. Retention Flow Sankey (new section or tab in Dominator Analysis)

**What:** A Sankey diagram where flow width = retained bytes.
Left side = GC root types, middle = top retainer classes, right = what
they retain. This gives the full "money trail" — from thread statics
through intermediate holders down to the objects actually consuming memory.

Currently the Leak Suspects section has a Sankey for merged paths, but
this would be a GLOBAL one covering all retention flows, not just suspects.

**Why it helps:** The existing dominator graph is undirected-looking and
cluttered. A Sankey forces a left→right flow reading that is much faster to
parse for "where does my memory come from".

```
 Retention Flow                                           [Show KB]
 ┌─────────────────────────────────────────────────────────────────┐
 │                                                                  │
 │  Thread locals ────╗                 ╔═══ InTxnImpl  2.7MB ─── │
 │              2.7MB ╠══ InTxnImpl ════╣                         │
 │                    ║                 ╚═══ HashMap    1.2MB ─── │
 │  Static fields ────╣                                            │
 │              2.5MB ╠══ LazyVals$ ════════ Object[]  2.5MB ─── │
 │                    ║                                            │
 │  JNI globals  ─────╣                 ╔═══ ZipFile$Src 1.3MB ── │
 │              1.5MB ╠══ Class ════════╣                         │
 │                    ║                 ╚═══ String    1.2MB ─── │
 │  System class ─────╝                                            │
 └─────────────────────────────────────────────────────────────────┘
  Hover node = select class  · Hover edge = exact bytes
```

**Data:** `gc_root_retained` (root type → classes) + `idom_pairs`
(class → dominated classes).
**Placement:** New "Sankey" tab inside Dominator Analysis graph panel.

---

## 10. Thread → Retained-Class Breakdown (enhancement to Threads section)

**What:** Each thread row expands to show a stacked bar of which CLASSES
it retains (not just total bytes). This answers "what is Thread-47 holding?"
without opening the object graph.

**Why it helps:** Multiple threads holding identical class mixes = one shared
data structure. One thread with a unique mix = that thread is the leak origin.

```
 Threads by Retained Heap                              [Show KB]
 ┌────────────────────┬──────────┬──────────────────────────────────┐
 │ Thread name        │ Retained │ Top retained classes             │
 ├────────────────────┼──────────┼──────────────────────────────────┤
 │▶ main              │ 302 KB   │ ▓▓▓▓▓ byte[] ▒▒▒ HashMap ░ ...  │
 │▶ Thread-259        │ 155 KB   │ ▓▓▓▓▓▓▓▓▓▓▓▓ Object[] ░░ ...   │
 │▶ Thread-172        │  29 KB   │ ████ InTxnImpl ██ String □ ...  │
 │▶ Thread-223        │  29 KB   │ ████ InTxnImpl ██ String □ ...  │
 │▶ Thread-230        │  29 KB   │ ████ InTxnImpl ██ String □ ...  │
 └────────────────────┴──────────┴──────────────────────────────────┘
  Thread-172/223/230 have identical class distribution → shared data structure
  [↓ Merge identical threads] — groups threads with matching top-5 class mix
```

**Data:** `thread_overview.threads` (locals) + `obj_graph_flat` dom subtree.
Can approximate from `thread_local_analysis`.
**Placement:** Inline bar added to existing threads table.

---

## 11. "Quick Path" Inspector Widget (enhancement to HeapInspector)

**What:** In the Inspector side panel, add a persistent "Retention Path"
minimap at the top of every view. It shows the 3–5 hop dominator chain
from the current object up to its GC root, always visible as a breadcrumb
with retained sizes. Each hop is a clickable button.

**Why it helps:** When drilling into an instance, you lose context of how
you got there. The minimap keeps the full context visible at all times.

```
 ┌─────────────────────────────────────────────────────────────────┐
 │ Inspector                                          [← Back] [✕] │
 ├─────────────────────────────────────────────────────────────────┤
 │ Retention path (GC root → this):                               │
 │  [Thread: main]  →  [InTxnImpl]  →  [HashMap]  →  [this obj]  │
 │   302 KB              2.7 MB          1.5 MB        248 B      │
 ├─────────────────────────────────────────────────────────────────┤
 │ java.util.HashMap                                               │
 │ Object #0x7f2a3b80  ·  shallow: 48 B  ·  retained: 1.5 MB      │
 │ ...                                                             │
```

**Data:** `obj_graph_flat` dom path (walk idom chain from current node to root).
**Placement:** Top of HeapInspector panel, persists across all inspector views.

---

## 12. Class Similarity Clusters (new section)

**What:** Group classes by structural similarity: classes with the same
retained/shallow ratio, same instance count order of magnitude, or that
frequently appear together in dominator chains. Display as a visual
cluster grid. Highlight "unusual" classes that don't fit any cluster
(anomaly detection).

**Why it helps:** In a complex heap, the leak is often ONE class that
doesn't belong with its neighbors. Clustering makes the outlier stand out.

```
 Class Clusters (k=5, by retained/instance profile)
 ┌────────────────────────────────────────────────────────────────┐
 │  Cluster A: "Cache entries" (high ret/instance, growing)       │
 │  ┌─────────────┐ ┌────────────┐ ┌──────────────────────┐      │
 │  │ InTxnImpl   │ │ LazyVals$  │ │  ZipFile$Source      │      │
 │  │ 94 inst     │ │ 1 inst     │ │  19 inst             │      │
 │  │ 29KB/inst ⚠ │ │ 2.5MB/inst │ │  69KB/inst ⚠        │      │
 │  └─────────────┘ └────────────┘ └──────────────────────┘      │
 │                                                                 │
 │  Cluster B: "Container overhead" (shallow ≈ retained)          │
 │  ┌─────────────┐ ┌────────────┐ ┌──────────────────────┐      │
 │  │ HashMap     │ │ HashMap$   │ │  Object[]            │      │
 │  │             │ │  Node      │ │                      │      │
 │  └─────────────┘ └────────────┘ └──────────────────────┘      │
 │                                                                 │
 │  Cluster C: "String infrastructure" (text/encoding overhead)   │
 │  ┌─────────────┐ ┌────────────┐ ┌──────────────────────┐      │
 │  │ String      │ │ byte[]     │ │  char[]              │      │
 │  └─────────────┘ └────────────┘ └──────────────────────┘      │
 └────────────────────────────────────────────────────────────────┘
  Click cluster = filter dominator graph to those classes
  ⚠ = anomaly: outlier from expected retained/instance for class type
```

**Data:** `biggest_classes` + `idom_pairs`. K-means on (log(instances),
log(retained/instances)) in the browser.
**Placement:** New section after Top Consumers.

---

## 13. Leak Score Dashboard (enhancement to Memory Triage / new landing section)

**What:** A single-screen "dashboard" card that scores each class on a
composite leak probability score (0–100), combining:
- retained/instance ratio vs median
- participation in dominator chains (is it a hub?)
- GC root distance (closer = more likely to be a leak anchor)
- depth below root (shallower = more likely to be the root cause)

Displayed as a ranked scorecard. Click any row to jump to that class in
all relevant sections.

**Why it helps:** Users currently have to cross-reference 4–5 different
tables to form a mental model. The score card does it automatically.

```
 Leak Score                                              [How scored?]
 ┌───┬────────────────────────────┬───────┬──────────────────────────────┐
 │ # │ Class                      │ Score │ Signals                      │
 ├───┼────────────────────────────┼───────┼──────────────────────────────┤
 │ 1 │ InTxnImpl                  │  94 ● │ ↑ret/inst  ↑hub  ↑depth      │
 │ 2 │ ZipFile$Source             │  87 ● │ ↑ret/inst  ↑depth            │
 │ 3 │ LazyVals$                  │  82 ● │ ↑ret/inst  ↑shallow-depth    │
 │ 4 │ SoftReference              │  71 ○ │ ↑hub  soft-ref-held          │
 │ 5 │ HashMap                    │  45 ○ │ ↑hub  normal-ratio           │
 │ 6 │ String                     │  12 · │ normal-ratio  low-depth      │
 └───┴────────────────────────────┴───────┴──────────────────────────────┘
  ● high confidence  ○ moderate  · likely benign
  Click row → highlights class in Dominator Graph, Triage, Inspector
```

**Data:** Derived from `biggest_classes`, `idom_pairs`, `triage`.
**Placement:** New card inside Memory Triage section, or standalone
"Leak Score" section.

---

## 14. Dominator Graph: Overlay Mode (enhancement to DomGraphView)

**What:** Add an "Overlay" dropdown to the dominator graph toolbar that lets
you color nodes by a secondary metric:
- **Default:** package-based color (current)
- **% Heap:** gradient from blue (low) to red (high % of heap)
- **B/instance:** retained bytes per instance (catches caches)
- **GC distance:** hops to nearest GC root (red = close to root = anchor)
- **Depth:** position in dominator tree depth (rainbow gradient)

**Why it helps:** The current graph is colored by package, which is
aesthetically nice but doesn't encode any leak-relevant information.
Switching to "% Heap" overlay immediately makes the biggest leaks pop
as bright red nodes.

```
 [Force] [Tree] | [All] [▲ Ret by] [▼ Retains] | [% Labels] [Edge Labels]
 Color: [Package ▼]  ← dropdown: Package / % Heap / B/instance / GC dist / Depth
        ┌─────────────────────────────────────────────────────────────────┐
        │  % Heap overlay:                                                 │
        │                          ●●  (InTxnImpl, 22.9%, red)           │
        │         ●               ● ●  (Object[], 9.5%, orange)          │
        │       ●   ●          ●                                          │
        │     ●       ●      ●   ●      (HashMap, 5.7%, yellow)          │
        │       ●         ●●●                                             │
        └─────────────────────────────────────────────────────────────────┘
  ■ red = >10%   ■ orange = 5–10%   ■ yellow = 1–5%   ■ blue = <1%
```

**Data:** `idom_pairs` + `biggest_classes` for node enrichment.
**Placement:** New "Color:" dropdown in DomGraphView toolbar.

---

## 15. Allocation Site → Retained Size Join (enhancement to Allocation Sites)

**What:** If allocation sites are present, join them with the retained-size
data: for each stack trace, show not just shallow bytes at allocation time,
but current retained bytes for objects allocated there. This reveals
"allocated here, never freed" patterns directly.

**Why it helps:** Allocation sites currently show shallow bytes at capture.
Joining with retained shows which allocation sites are contributing to live
memory pressure — the leak origin.

```
 Allocation Sites                              [Filter by class…]
 ┌────────────────────────────────────────────────────────────────┐
 │ #1  InTxnImpl  94 objects  13 KB shallow  2.7 MB retained ⚠   │
 │     scala.concurrent.stm.ccstm.InTxnImpl.<init>(InTxnImpl.sc:42│
 │       ← scala.concurrent.stm.ccstm.TxnLevelImpl.begin(:88)    │
 │         ← scala.concurrent.stm.ccstm.CCSTMExecutor.apply(:201) │
 │     Retained/allocated: ████████████████████████ 20,800×       │
 │     [Inspect class]  [Show in dominator graph]                 │
 ├────────────────────────────────────────────────────────────────┤
 │ #2  ZipFile$Source  19 objects  1 KB shallow  1.3 MB retained ⚠│
 │     java.util.zip.ZipFile$Source.<init>(ZipFile.java:581)      │
 │     [Inspect class]  [Show in dominator graph]                 │
 └────────────────────────────────────────────────────────────────┘
  ⚠ = retained >> shallow (object still live, data accumulated after alloc)
```

**Data:** `alloc_sites` joined with `biggest_classes` by class name.
**Placement:** Enhancement to existing AllocSitesSection.

---

## Implementation Priority

Ranked by impact vs effort:

| # | View | Impact | Effort | Notes |
|---|------|--------|--------|-------|
| 14 | Overlay mode (color by % heap, B/inst) | ★★★★★ | Low | Just changes node color mapping |
| 5 | B/instance ⚠ flag in classes table | ★★★★☆ | Low | One computed column |
| 6 | GC root heatmap matrix | ★★★★☆ | Medium | Needs data join |
| 13 | Leak score dashboard | ★★★★☆ | Medium | Computed from existing data |
| 11 | Quick path breadcrumb in Inspector | ★★★★☆ | Medium | Already have idom chain |
| 1 | Retention flamegraph | ★★★★☆ | High | New D3/canvas component |
| 7 | Blame view in DomGraph | ★★★★☆ | Medium | Max-path walk in existing graph |
| 10 | Thread → class breakdown bars | ★★★☆☆ | Medium | Needs thread-local data join |
| 4 | Path between two classes | ★★★☆☆ | Medium | BFS over idom_pairs |
| 9 | Global Sankey | ★★★☆☆ | High | Already have sankey library |
| 2 | Heatmap matrix | ★★★☆☆ | Medium | Canvas heatmap |
| 8 | Instance scatter plot | ★★★☆☆ | Medium | Needs per-instance data |
| 12 | Class clusters | ★★☆☆☆ | High | K-means in browser |
| 3 | Class relationship diagram | ★★☆☆☆ | Medium | Redundant with current graph |
| 15 | Alloc site + retained join | ★★★☆☆ | Low | Simple table join |

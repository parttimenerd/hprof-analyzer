# hprof-analyzer Algorithm Design Decisions

## 1. Shallow Size Formulas

All formulas follow MAT parity as implemented in hprof-redact `HeapGraphBuilder.java`.

**Key values:**
- `pointerSize = idSize` (from HPROF header, 4 or 8)
- `refSize = idSize` unless compressed OOPs detected (then 4)
- `objectAlign = 8` always

**Compressed OOPs detection (id_size == 8 only):**
Track `previousArrayStart` and `previousArrayUncompressedEnd = prevStart + 16 + numElem*8`.
If any OBJ_ARRAY_DUMP address falls inside `(prevStart, prevUncompressedEnd)`, set `found_compressed = true` → `refSize = 4`.

**Instance objects:**
```
alignUp(calculateSizeRecursive(clazz), objectAlign=8)
calculateSizeRecursive(c) =
  if c has no super: pointerSize + refSize
  else: alignUp(ownFieldsSize(c) + calculateSizeRecursive(super), refSize)
ownFieldsSize(c) = ownObjectFieldCount(c)*refSize + ownPrimitiveFieldBytes(c)
```
Store `ownObjectFieldCount` and `ownPrimitiveFieldBytes` per class during CLASS_DUMP scan.
This REPLACES the raw `instance_size` from the HPROF header (MAT recalculates it).

**Object arrays:**
```
alignUp(pointerSize + refSize + 4 + numElem * refSize, objectAlign)
```

**Primitive arrays:**
```
alignUp(alignUp(pointerSize + refSize + 4, refSize) + numElem * elemSize, objectAlign)
```

**Class objects (java.lang.Class instances):**
```
alignUp(staticObjectFieldCount * refSize + staticPrimitiveFieldBytes, objectAlign)
```
Class objects get classIndex = java/lang/Class (not their own class).

**Minimum size (fallback):**
```
alignUp(pointerSize + refSize, objectAlign)
```

## 2. Dominator Tree Algorithm

**Algorithm: Cooper-Harvey-Kennedy (CHK) iterative dataflow** (NOT Lengauer-Tarjan).

hprof-redact uses CHK from the 2001 paper. MAT uses Lengauer-Tarjan, but since hprof-redact
validates against MAT and uses CHK, we use CHK as well. CHK is simpler to implement correctly.

**Key implementation details:**
- Index space: 0 = virtual root, 1..N-1 = objects (1-indexed, matching idMap slot+1)
- `idom[VIRTUAL_ROOT] = VIRTUAL_ROOT` (self-loop sentinel)
- `idom[v] = UNDEFINED (-1)` for unprocessed/unreachable nodes
- `vrAdjacent` bitset: GC roots have implicit predecessor from virtual root, pre-seeded to `idom[v] = VIRTUAL_ROOT`
- Process RPO order, skip virtual root (rpoOrder[0])
- For each node b: seed newIdom with VIRTUAL_ROOT if vrAdjacent[b], then process inbound CSR predecessors
- `intersect(b1, b2)`: finger-walk both nodes up via idom using rpoPos to guide direction
- Iterate until convergence (must converge in ≤ N passes for correct RPO)
- After convergence, idom[gcRoot] = VIRTUAL_ROOT

**Inbound CSR stores predecessors as sorted, VByte-delta-encoded indices.**
Excluded edges (see §4) are stored with `srcIdx | INT_MIN_VALUE` marker and skipped during dominator computation.

## 3. GC Root Normalization

**Deduplication:** Use a BitSet indexed by object index. Each address added to GC roots only once regardless of how many root records reference it.

**Root types processed (from HPROF sub-tags):**
- ROOT_UNKNOWN (0xFF), ROOT_JNI_GLOBAL (0x01), ROOT_JNI_LOCAL (0x02), ROOT_JAVA_FRAME (0x03)
- ROOT_NATIVE_STACK (0x04), ROOT_STICKY_CLASS (0x05), ROOT_THREAD_BLOCK (0x06)
- ROOT_MONITOR_USED (0x07), ROOT_THREAD_OBJ (0x08)

**System class root fallback:**
If no STICKY_CLASS roots were emitted in the dump, treat all non-array classes loaded by the
boot classloader (classLoader == 0) as implicit GC roots (MAT parity: `addSystemClassRootsIfMissing`).
Detection: track `has_sticky_class_roots` during pass1; apply fallback in pass2 graph building.

**Thread-local synthetic edges:**
JNI_LOCAL and JAVA_FRAME roots are associated with thread objects via thread serial numbers.
hprof-redact adds synthetic edges from thread object → local. For MAT parity we should implement
this, but if needed for first pass can omit (may affect dominators of thread-local objects).

## 4. Exclude-Pair Edges

Three pairs excluded from dominator computation (edges NOT followed when computing reachability/dominators):
1. `java/lang/ref/Reference` field `referent`
2. `java/lang/ref/Finalizer` field `unfinalized`
3. `java/lang/Runtime` field `<Unfinalized>`

**Implementation:** During pass2 edge scanning, when building inbound CSR:
- Identify the class index and field name index for each of the 3 pairs
- Mark excluded inbound edges with `srcIdx | INT_MIN_VALUE` (high bit set) in inbound CSR data
- During dominator computation (CHK): skip predecessors with high bit set
- During retained size computation: use normal edges (exclude pairs still "count" for retained)

## 5. Class Retained Heap (for Histogram)

**Definition (MAT top-ancestor semantics):**
For class C in the histogram, the retained heap is NOT `sum(retained[v] for v where class[v] == C)`.

Instead, for each class C:
1. Collect all objects of class C **plus** the class-object for C
2. Find the "top ancestors" of this set in the dominator tree:
   objects in the set whose immediate dominator is NOT in the set
3. Sum their retained sizes

**Efficient O(N) implementation:**
During retained-size computation, in a forward DFS of the dominator tree, maintain a
`classToLastDepth` array tracking whether any ancestor of the current node has the same class.
Set `hasSameClassAncestor[v] = true` if classToLastDepth[class[v]] > 0 or classObjDepth[class[v]] > 0.

In the histogram aggregation:
- For each object v of class C: if `!hasSameClassAncestor[v]`, add `retained[v]` to class C's total

## 6. Leak Suspects (FindLeaksQuery logic)

**Threshold:** `threshold = threshold_percent * totalHeap / 100`
- `threshold_percent = 20` (FindLeaksQuery default, called from LeakHunterQuery with 10)
- When called from LeakHunterQuery: `threshold_percent = threshold_percent` (default 10)
- Use 10% of `totalHeap = snapshot.getSnapshotInfo().getUsedHeapSize()` = sum of all shallow sizes

**Algorithm:**
1. Get top-level dominators = objects where `idom[v] == VIRTUAL_ROOT`, sorted by retained desc
2. **Single suspects:** all top-level dominators with `retained[v] > threshold` (while loop from top)
3. **Class-group suspects:** from remaining top-level dominators, group by class; if group retained > threshold, add as group suspect (no exclusion needed for already-found single suspects — MAT code comment says "No need to avoid")
4. Sort all suspects (single + group) by retained descending
5. Both single and class-group suspects get an **accumulation point** (optional):
   - Walk dominated children (largest retained first): `idom[child] == v`, pick child with max retained
   - If `child.retained / parent.retained >= big_drop_ratio (0.7)`, continue walk
   - Stop when ratio drops or no children → accumulation point = last stop
   - MAX_DEPTH = 1000

## 7. Top Consumers (TopConsumers2Query logic)

**Threshold:** `thresholdPercent = 1` → `threshold = 1% * totalHeap`
(totalHeap = sum of shallow of all objects, same as useHeapSize in MAT)

**Sections:**
1. **Biggest Objects** — top-level dominators with `retained > threshold` (sorted desc)
2. **Biggest Classes by Retained Heap** — group top-level dominators by class, sum retained, filter `> threshold`
3. **Biggest Class Loaders** — group by classloader (skip for first version)
4. **Package Tree** — extract package from dot-notation class name (MAT uses `.` separator):
   - Split on `.` and walk tree hierarchy
   - Arrays (`[B`, `[Ljava/lang/Object;` etc.) → class name is the element type or array notation
   - Filter packages with `retained > threshold`

**Package extraction rule:**
- Class names in HPROF use slash-notation (`java/lang/String`), but packages use dot-notation
- Convert: `java/lang/String` → package `java.lang`
- Primitive arrays (`[B`, `[I`, etc.) → use class name as-is in top-level `<default>` package
- Object arrays (`[Ljava/lang/String;`) → extract element class, use its package

## 8. System Overview

**Heap Summary table fields:**
- HPROF format (string from header)
- File size (bytes, formatted as MB/GB)
- Total objects (all objects in idMap, including unreachable)
- Total shallow heap (sum of all shallow sizes)
- GC roots (count after dedup)
- Classes loaded (class_dump_count)
- Unreachable objects (objects with idom == UNDEFINED; not needed for first pass)

**Class Histogram:**
- Top 50 classes by retained heap
- Columns: #, Class, Instances, Shallow Heap, Retained Heap
- Retained heap uses top-ancestor semantics (§5)

---

# Output, CLI, Schema & Validation Contract

Sections 1–8 above document the *analysis algorithm*. The sections below
document the *output contract*: the CLI surface, the two output formats, the
canonical JSON schema and its versioning policy, and the MAT↔JSON validation
methodology that proves the numbers are correct. This describes **what the tool
does today**; not-yet-shipped work is marked *planned*.

## 9. CLI surface

The binary is a subcommand CLI (clap):

```
hprof-analyzer <COMMAND>

  analyze  Analyze a heap dump and write a report (Markdown or JSON)
  diff     Compare a MAT report against our canonical JSON (exit 2 on FAIL)
  render   Render a saved canonical Report JSON to Markdown or JSON
  dev      Developer / diagnostic commands
```

### `analyze <INPUT> [OUTPUT]`

Parse a dump and emit the report. Writes to `OUTPUT` if given, else stdout.

| Option | Values | Default | Meaning |
|---|---|---|---|
| `-f, --format` | `md`, `md-graphs`, `html`, `json` | `md` | Output format. `md` is the byte-exact MAT-parity path; `md-graphs` adds ASCII bars/sparklines/trees; `html` is a self-contained page; `json` is the canonical model. |
| `--detail <LEVEL>` | `minimal`, `default`, `max` | `default` | Output-size preset controlling seven caps (see below). `default` reproduces the historical cap values. |
| `-v, --verbose` | flag | off | Per-phase timing + RSS to stderr. |
| `--trace-rss` | flag | off | Enable the RSS-probe instrumentation. |

The internal cold-array codec is hardwired to Deflate9 (the former `-c/--compress`
switch was removed: `none` defeated the pass-2 early-compress and cost ~4 GB RSS on
the big dump, so it is no longer selectable).

**Heavy analyses (always on).** Four analyses run unconditionally and each adds an
additive `Option<T>` section to every format:

| Analysis | Produces |
|---|---|
| Root paths | Dominator chain from each single-object suspect up to its GC root (the same dominator-based "path to the accumulation point" MAT's Leak Suspects report shows). |
| Allocation sites | Objects aggregated by allocation stack-trace serial; honest empty note when the JVM recorded none. |
| Thread locals | Each thread's local-root object sample. |
| Dominator subtree | Full multi-level dominator subtree per accumulation point. |

**`--detail` preset.** A single flag scales seven output-size caps. `default` reproduces
the historical cap values (so the JSON/golden snapshots and internal cap behavior match
what the individual cap flags produced before they were removed):

| `--detail` | root depth | alloc top | thread locals | dom nodes | dom depth | leak children | top consumers |
|---|---:|---:|---:|---:|---:|---:|---:|
| `minimal` | 10 | 15 | 5 | 500 | 10 | 15 | 10 |
| `default` | 30 | 50 | 20 | 5,000 | 20 | 50 | 20 |
| `max` | 200 | 500 | 100 | 100,000 | 50 | 500 | 100 |

`--detail max` raises the dominator-tree cap to 100k nodes, which may push peak RSS above
the default ceiling on very large dumps — a deliberate, documented trade-off (§14). The
Markdown MAT-parity view is unaffected by the removal of the flags: `--detail default`
reproduces the historical cap values, and the ours-only sections (root paths, dominator
subtree, thread locals, allocation sites) now always appear in the Markdown output.

### `render <INPUT> [-f md|json]`

Offline: read a previously saved canonical Report JSON (or `-` for stdin) and
re-render it — **no heap access**. `render` reads `schema_version` and refuses
to render a JSON whose version does not match this binary's `SCHEMA_VERSION`,
rather than silently mis-rendering.

### `compare mat <MAT> <OURS> [-f md|json]`

The MAT↔JSON **validator**: parse a MAT-exported report and compare it
field-by-field against our canonical JSON. Exits `2` on any FAIL. See §12. This
is a *correctness* comparator against an external oracle — distinct from the
cross-dump growth diff (`compare reports`).

### `dev <COMMAND>`

Diagnostic subcommands (not the stable user surface):

- `emit-schema` — print the schemars-derived JSON Schema; used to keep the
  committed `schema/report.schema.json` in sync (a test asserts equality).
- `sweep-aggregate <DIR>` — fold per-dump `*.diff.json` files into one gate
  report; exits `2` on gate-fail (§12.3).
- `dump-pass1 <INPUT>` — dump pass-1 parse stats (counts, class histogram) as
  JSON, for diagnostics.

### Cross-dump growth diff

- **`compare reports A.json B.json`** — pure offline post-processing of two canonical
  Report JSONs (per-class Δinstances/Δretained, new/grown suspects, growth leaders);
  the "is it growing over time?" signal. Distinct from the `compare mat` comparator.

## 10. Output formats

### 10.1 Markdown (default)

The `md` path is the MAT-parity report. Its H2/H3 structure is asserted by
`tests/integration.rs`; its exact bytes are pinned by `tests/parity.rs` against
`tests/fixtures/dump_*_ours.md` for 8 dumps. The only excluded line is the
`Generated by ...` timestamp. Tables are column-aligned via `src/md.rs`.

Sections: **System Overview** (Heap Summary, Class Histogram, GC Roots by Type,
Heap Composition), **Leak Suspects** (plain-language list with a why-alive path
+ GC-root type), **Top Consumers** (Biggest Objects / Classes / Packages),
preceded by an **OOM Triage** lead-in (Shape, One-leak-or-many).

### 10.2 Canonical JSON

`analyze --format json` serializes the `Report` struct with serde. The model is
**deterministic by construction**:

- No `HashMap` in the serialized model — all collections are ordered/sort-stable,
  so field and element order is fixed.
- No `f64` in the JSON — percentages are `#[serde(skip)]` (Markdown-only) or
  carried as integer **basis points** (100 bp = 1%), avoiding float
  nondeterminism.
- A determinism test serializes a fixture twice and asserts byte-identical
  output; a golden-snapshot test asserts a fresh run equals a committed golden
  (modulo the two run-varying fields `generated` and `overview.file_path`).

Top-level shape:

```jsonc
{
  "schema_version": 2,
  "generated": "2026-07-13T…Z",   // per-run UTC timestamp
  "overview": { … },              // SystemOverview
  "leaks":    { … },              // LeakSuspects
  "top":      { … }               // TopConsumers
}
```

Optional/phased sections (system properties, threads, top components, deep
reference data, allocation sites) are **absent** in default output today and
appear only when their phase/flag is active (§13).

## 11. Size budget — no unbounded per-object arrays (INVARIANT)

A dump can hold hundreds of millions of objects. **The canonical JSON must never
contain a `Vec` that grows with the object count.** Every list is bounded by:

- the number of loaded **classes** (the histogram — one row per class), or
- a fixed **top-N / threshold cap** (top consumers, biggest objects, suspects),
  or
- the longest **dominator chain** (the dominator-depth histogram).

Enforced by `tests/json_size_budget.rs`: on the largest fixture the serialized
JSON size tracks *classes*, not *objects* (~172 bytes/histogram-row; a leaked
per-object array would blow past a class-bounded byte budget). Planned Phase-E
component sub-lists and allocation sites MUST likewise be threshold/top-N bounded
— this guard will enforce it.

## 12. MAT ↔ JSON validation methodology

Because MAT is the reference implementation, we prove our numbers by comparing
our canonical JSON against MAT's exported report, field by field, under a
**zero-tolerance** policy: a field is correct only if it MATCHES exactly, or its
divergence is **programmatically proven** to be one of a small enumerated set of
benign reasons. Anything else FAILs.

### 12.1 The 3-tier classifier (`src/diff.rs`, `enum Tier`)

- **MATCH** — exact equality. Zero default tolerance — no epsilon.
- **EXPLAINABLE(reason)** — not equal, but proven to be a whitelisted benign
  reason (below), carrying evidence.
- **FAIL** — anything else. A missing set member is FAIL, never "reorder."

### 12.2 The EXPLAINABLE whitelist (`enum Explanation`)

Every accepted reason is enumerated in code and exercised by classifier tests.
New reasons require a new programmatic check — never a silent tolerance:

1. **Order** — collections differ in iteration order but are equal *as sets*.
   *(Constructed in tests; the runtime per-class comparison keys by name and is
   already order-agnostic.)*
2. **TieBreak** — stable-sort tie-break on entries with **identical** sort keys.
3. **Rounding** — MAT display rounding / unit truncation. MAT formats bytes with
   `DecimalFormat("#,##0.#")` (1024-based, ≤1 fractional digit, trailing zeros
   dropped, HALF_EVEN). The classifier parses MAT's shown value into the
   inclusive byte-band `[center − half·scale, center + half·scale]` it could
   represent at its own precision and accepts iff our exact value lies inside —
   rounding-mode-agnostic, so the gate stays honest without loosening.
4. **NoCounterpart** — a MAT-only or ours-only field with no counterpart;
   skipped.
5. **MatClassObjectRootingGap** — the `total_objects` / `classes_loaded` /
   `total_shallow` divergence proven localized entirely to `java.lang.Class`-
   object reachability (the known root-frontier difference). Valid only when the
   per-class histogram proof holds.

### 12.3 The sweep gate (`dev sweep-aggregate`, `N_MIN`)

`compare mat` writes a per-dump `*.diff.json`; `sweep-aggregate` folds a directory of
them into one verdict. **The gate PASSES only if BOTH hold:**

- **zero FAILs** across all dumps, AND
- **≥ `N_MIN` (= 15) REAL MAT comparisons** ran (dumps without a MAT reference do
  not count toward `N_MIN`).

The report lists the real-comparison count, the full proven-EXPLAINABLE audit
with evidence, and the full FAIL list. This is a hard gate, not advisory.

### 12.4 Known open item

The `scala.collection.immutable.$colon$colon` duplicate-histogram-row divergence
(a spurious second class row from an upstream class-attribution edge case) is the
sole open validation FAIL, tracked separately. It is NOT accepted by the
whitelist — it is a real bug, not a benign reason.

## 13. Schema & versioning contract

The model derives its JSON Schema via `schemars`; it is committed at
`schema/report.schema.json`, and a test asserts `dev emit-schema` equals the
committed file (values). Emitted JSON is validated against the schema via the
`jsonschema` dev-dependency (draft 2020-12).

`SCHEMA_VERSION` is currently **2**. The policy covers **structure AND
semantics**, because schemars cannot encode units or meaning:

- **Additive** (new `Option<T>` field) → keep `schema_version`; older consumers
  still validate.
- **Breaking** (rename/remove/retype a required field) → bump `SCHEMA_VERSION`
  and regenerate the committed schema.
- **Semantic** (units/meaning change even with identical shape — e.g. `retained`
  switches bytes↔KB, a percentage base changes, a basis-points interpretation
  changes) → **also** bump `SCHEMA_VERSION`. A shape-only check cannot catch
  this, so it is a documented policy obligation.

`render` reads `schema_version` and refuses to render a JSON whose version does
not match this binary's `SCHEMA_VERSION`.

**Dependency scope:** serde, serde_json, schemars are runtime deps used **only in
the report phase**; jsonschema is a dev-dep. None are referenced in the
byte-exact pass1/pass2 pipeline, so they add no analysis RSS/runtime cost. The
Markdown default path serializes nothing — it renders directly from the model —
and is guarded by the byte-exact parity test.

## 14. RSS & pipeline constraints

- Peak working-set RSS on the 35.8 GB reference dump must stay ≤ **14369 MB**;
  every analysis-pipeline commit records the measured big-dump peak RSS + runtime.
- Analysis output is **byte-exact** (excluding the `Generated by ...` line).
- No threading, no mmap, no spilling in-memory data to disk to cut RSS — RSS is
  reduced structurally.
- Report-phase features (JSON, OOM triage, validation, docs) do **not** touch the
  analysis pipeline and carry no analysis-RSS impact. The four always-on heavy analyses
  (§9) add bounded per-object capture (allocation sites / thread locals) or reuse
  already-resident structures (root paths walk the `idom` array, the dominator subtree
  reuses the dominator-children CSR), so none preserves an extra large array across the
  peak — the ceiling still holds with all four running.
- **Big-dump peak (all four analyses on):** on the 35.8 GB reference dump the root-path
  chain is derived from the dominator tree (the same basis as MAT's Leak Suspects path),
  so it needs no inbound-referrer CSR preserved past the dominator phase — the earlier
  literal-referrer design decompressed the full ~6.4 GB CSR just to walk a few hundred
  nodes, spiking peak to 25.46 GiB. The current always-on path measured 14.21 GiB, under
  the ceiling. `--detail max` raises the dominator-tree node cap to 100k, which can push
  peak higher — a documented trade-off.

## 15. Testing strategy (summary)

- **Parity** — 8 dumps, Markdown output compared against committed baselines
  (`tests/parity.rs`); only the `Generated by` line excluded. Asserts output
  stability under `--detail default` (the baselines include the ours-only
  always-on sections, so they are no longer byte-identical to MAT).
- **Structural integration** — Markdown heading/section/table structure
  (`tests/integration.rs`).
- **MAT↔JSON sweep** — the §12 zero-tolerance classifier + `N_MIN` gate; the
  correctness centerpiece.
- **JSON golden snapshot** — a fresh run equals a committed golden modulo
  `generated` + `overview.file_path`.
- **Round-trip** — `from_str(to_string(report)) == report`, and
  Graph→Report→JSON→Report→MD equals Graph→Report→MD.
- **Schema value-equality** — `schema_for!(Report)` equals the committed schema;
  `dev emit-schema` equals the file.
- **Schema validation** — emitted JSON validates against the committed schema.
- **Version guard** — emitted `schema_version` equals `SCHEMA_VERSION`.
- **Determinism guard** — serialize a fixture twice → byte-identical.
- **Size-budget guard** — no unbounded per-object array (§11).

## 16. Planned phases (not yet shipped)

- **Phase E — new-pass parsing:** class loaders, allocation stacks (TRACE/FRAME),
  threads, system properties, per-loader Top Components. Populates the currently-
  absent optional sections. Touches the parse pipeline → RSS must be measured.
- **Phase F — literal reference paths:** bounded compressed reference CSR + edge
  names for literal GC-root paths (not the dominator-based approximation shipped
  today), referrer-diversity, reference-type reachability stats. Uncapped RAM (§9).
- **Phase H — HTML report:** a self-contained single-file HTML report (charts,
  offline); the committed JS bundle is rebuilt + size-checked in CI.

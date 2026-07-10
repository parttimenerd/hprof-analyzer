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

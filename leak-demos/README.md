# Java Heap Dump Demo Suite

A collection of **34 self-contained Java programs** that demonstrate every major
category of Java heap dump analysis: memory leaks, collection-efficiency waste,
duplicate detection, structural overhead, edge cases, unreachable objects, and
normal healthy application profiles.

Designed as the test corpus for **[hprof-analyzer](https://github.com/parttimenerd/hprof-analyzer)**.

---

## Quick Start

```bash
# Compile & run a single scenario (any Java 11+ JDK):
java -Xmx64m leak-demos/01_StaticCacheLeak.java

# Capture heap dumps for ALL scenarios:
cd leak-demos
./capture_all.sh

# Capture only the normal-app scenarios:
./capture_all.sh normal

# Capture a specific scenario by number:
./capture_all.sh 31

# Analyse all collected dumps:
./capture_all.sh analyse
```

Dumps are written to `leak-demos/dumps/`.  
Logs are written to `leak-demos/dumps/logs/`.

---

## capture_all.sh Reference

```
Usage: ./capture_all.sh [filter …]

Filter arguments
  (none)          Run all 34 scenarios
  oom             Leak / OOM scenarios only           (01–17)
  waste           Collection-efficiency waste         (18–22)
  normal          Normal healthy-app scenarios        (23–27, 31–34)
  unreachable     Unreachable-objects demo            (30)
  analyse         Summarise existing dumps via hprof-analyzer (no new captures)
  01  31  …       One or more scenario numbers (run exactly those)

Environment overrides
  JAVA        Path to java binary     (default: java from PATH)
  JMAP        Path to jmap binary     (default: jmap from PATH)
  JCMD        Path to jcmd binary     (default: jcmd from PATH)
  DUMPS_DIR   Output directory        (default: ./dumps)

Dump methods used
  OOM scenarios      → -XX:+HeapDumpOnOutOfMemoryError (captured automatically)
  Idle/live scenarios → jcmd GC.heap_dump (triggers GC → live objects only)
  Scenario 30        → jmap -dump:format=b (NO live filter → includes unreachable)
```

---

## Prerequisites

| Requirement | Notes |
|-------------|-------|
| Java 11+ | Any distribution (SapMachine, OpenJDK, GraalVM, …) |
| `jcmd` + `jmap` | Included in every JDK; must be on `PATH` |
| `hprof-analyzer` | Optional, for the `analyse` command |

**Install hprof-analyzer** (macOS / Linux):

```bash
brew tap parttimenerd/hprof-analyzer
brew install hprof-analyzer
# or download from https://github.com/parttimenerd/hprof-analyzer/releases
```

---

## Scenario Reference

### Part 1 — Leaks & Out-of-Memory Errors

These scenarios terminate with `OutOfMemoryError`.  
Run with `-XX:+HeapDumpOnOutOfMemoryError -XX:HeapDumpPath=dump.hprof`.

| # | File | Leak pattern | Key class / root cause |
|---|------|-------------|------------------------|
| 01 | `01_StaticCacheLeak.java` | Static `HashMap` accumulates request objects indefinitely | `StaticCacheLeak$RequestContext` |
| 02 | `02_ListenerLeak.java` | Event listeners registered but never removed | `ListenerLeak$Listener` |
| 03 | `03_InnerClassLeak.java` | Anonymous inner-class instances hold outer-class references | `InnerClassLeak$1` |
| 04 | `04_UnboundedCollectionLeak.java` | Unbounded `LinkedList` grows with every request | `UnboundedCollectionLeak$Request` |
| 05 | `05_ThreadLocalLeak.java` | `ThreadLocal` values never removed from pooled threads | `ThreadLocalLeak$Context` |
| 06 | `06_ExecutorLeak.java` | `ExecutorService` tasks queue faster than they execute | `Runnable` impl |
| 07 | `07_ShutdownHookLeak.java` | Shutdown hooks registered in a loop hold live references | `ShutdownHookLeak$Hook` |
| 08 | `08_ClassLoaderLeak.java` | Custom `ClassLoader` instances not unloaded → Metaspace OOM | `ClassLoaderLeak$CustomLoader` |
| 09 | `09_ClassGenerationOOM.java` | Generates classes at runtime until Metaspace is exhausted | Generated `Clazz$N` classes |
| 10 | `10_FinalizerQueueLeak.java` | Objects with `finalize()` pile up in `Finalizer` queue | `FinalizerQueueLeak$Heavy` |
| 11 | `11_WeakHashMapLeak.java` | `WeakHashMap` used with value holding key → prevents GC | `WeakHashMapLeak$Holder` |
| 12 | `12_SoftReferenceCacheLeak.java` | Soft-reference cache retains objects under GC pressure | `SoftReferenceCacheLeak$BigBlob` |
| 13 | `13_DirectBufferLeak.java` | `ByteBuffer.allocateDirect()` not freed → native OOM | `java.nio.DirectByteBuffer` |
| 14 | `14_GcOverheadOOM.java` | Allocates tiny objects faster than GC can reclaim → CPU-spin OOM | `byte[]` |
| 15 | `15_NativeThreadOOM.java` | Spawns thousands of threads → OS thread limit exhausted | `java.lang.Thread` |
| 16 | `16_ArraySizeOOM.java` | `new int[Integer.MAX_VALUE - 4]` → instant OOM | `int[]` |
| 17 | `17_PersistenceContextLeak.java` | Simulated JPA persistence context caches every entity | `PersistenceContextLeak$Entity` |

**Example analysis**:
```bash
# Capture
java -Xmx64m -XX:+HeapDumpOnOutOfMemoryError -XX:HeapDumpPath=01.hprof \
     leak-demos/01_StaticCacheLeak.java

# Triage
hprof-analyzer heap summary 01.hprof
hprof-analyzer heap report  01.hprof --section leaks
```

---

### Part 2 — Collection Efficiency Waste

These scenarios stay alive and are dumped with `jcmd GC.heap_dump` (live objects only).

| # | File | Waste pattern | What to look for |
|---|------|--------------|------------------|
| 18 | `18_OversizedHashMapWaste.java` | `HashMap` pre-allocated to `Integer.MAX_VALUE / 2` capacity for 10 K entries | Backing `Object[]` with fill ratio ≈ 0.001% |
| 19 | `19_SparseMapAfterRemoval.java` | 1 M items inserted into `HashMap`, then 99% removed; internal table not trimmed | Large internal table, very few live entries |
| 20 | `20_ArrayListCapacityWaste.java` | `ArrayList` grown by single appends to 5 M capacity, then trimmed to 1 K | Backing `Object[]` still has old capacity |

**Example analysis**:
```bash
hprof-analyzer heap report 18.hprof --section collections
hprof-analyzer heap report 19.hprof --section triage
```

---

### Part 3 — Duplicate Detection

| # | File | Pattern | What to look for |
|---|------|---------|-----------------|
| 21 | `21_DuplicateStringWaste.java` | 500 K `String` objects with only ~100 distinct values; no interning | hprof-analyzer duplicate-strings report |
| 22 | `22_DuplicateByteArrayWaste.java` | 200 K `byte[]` with identical content copied per-object | hprof-analyzer duplicate byte-arrays report |

**Example analysis**:
```bash
hprof-analyzer heap report 21.hprof --find-duplicates
hprof-analyzer heap report 22.hprof --find-duplicates
```

---

### Part 4 — Object Structure & Overhead

| # | File | Pattern | What to look for |
|---|------|---------|-----------------|
| 23 | `23_NormalDomainObjectGraph.java` | 50 K `Order` objects, each with `Customer`, `Address[]`, `LineItem[]` | Deep ownership tree; typical domain-model overhead |
| 24 | `24_MetricTimeSeries.java` | 1 M `DataPoint(long ts, double value)` objects in a `List` | Boxed `Double`/`Long` vs. primitive array trade-off |
| 25 | `25_DeepObjectChain.java` | Linked list 100 K nodes deep; also a fully recursive binary tree | Retention paths: GC root → chain tip |
| 26 | `26_BoxedPrimitivesWaste.java` | `List<Integer>` vs `int[]` side-by-side; 1 M elements each | Object header overhead on `Integer` |
| 27 | `27_ManySmallObjects.java` | 2 M tiny 2-field objects; heap dominated by header overhead | `java.lang.Object` header count in histogram |

---

### Part 5 — Edge Cases

| # | File | Pattern | What to look for |
|---|------|---------|-----------------|
| 28 | `28_StringInternAbuse.java` | `String.intern()` called on 500 K unique strings → String pool overflow | String pool size via hprof summary |
| 29 | `29_HashMapHighCollisionRate.java` | Keys return constant `hashCode()` → all in one bucket → O(n) `HashMap` | Treeified bucket depth in HashMap |

---

### Part 6 — Unreachable Objects

> **Dump method matters here.**  
> `jcmd GC.heap_dump` triggers a GC first — unreachable objects vanish.  
> `jmap -dump:format=b` does NOT trigger GC — dead objects are still in the dump.  
> Use `-all=true` flag on `jcmd GC.heap_dump` to include unreachable objects.

| # | File | Pattern | Notes |
|---|------|---------|-------|
| 30 | `30_GcOverwhelmedGarbage.java` | Creates 150 K `GarbageRequest` objects, drops all references, then idles | Run with SerialGC on a large heap to delay GC; dump immediately with `jmap -dump:format=b` |

**Demonstrating the difference**:
```bash
java -Xmx256m -Xms256m -XX:+UseSerialGC \
     -XX:NewSize=160m -XX:MaxNewSize=160m \
     leak-demos/30_GcOverwhelmedGarbage.java &

PID=$!
# Wait for "DUMP WINDOW OPEN"
sleep 15

# Dump WITH unreachable objects (jmap without live):
jmap -dump:format=b,file=30_all.hprof $PID

# Dump WITHOUT unreachable objects (jcmd triggers GC):
jcmd $PID GC.heap_dump 30_live.hprof

kill $PID

# Compare:
hprof-analyzer heap summary 30_all.hprof   # much larger
hprof-analyzer heap summary 30_live.hprof  # only live objects
```

---

### Part 7 — Normal Applications

Four realistic applications with **no bugs or leaks**. Use them to practise
answering: *"Where does all this memory actually go?"*

| # | File | Domain | Key structures |
|---|------|--------|---------------|
| 31 | `31_AppWordFrequency.java` | Text corpus / NLP | 3 K `Document` objects; global term-frequency `HashMap`; inverted index; per-doc TF-IDF weights; top-500 word list |
| 32 | `32_AppSessionStore.java` | Web application | 10 K `Session` objects; `UserProfile` per session; LRU eviction `Deque`; per-role and per-region counters |
| 33 | `33_AppEventPipeline.java` | Stream processing | 50 K `Event` sliding window; per-minute `TreeMap` bucket aggregates; `AlertRule` list; 1 K alert ring buffer |
| 34 | `34_AppProductCatalog.java` | E-commerce | 8 K `Product` objects; reviews; `CategoryNode` tree; price `TreeMap` index; keyword `HashMap` search index |

**Suggested analysis questions**:

```bash
# What is the biggest contributor to heap usage?
hprof-analyzer heap histogram 31.hprof

# How many distinct String values exist vs how many String instances?
hprof-analyzer heap report 31.hprof --find-duplicates

# Show the top-5 most expensive collections by retained size:
hprof-analyzer heap report 32.hprof --section collections

# Show the full object graph from the top-level static field:
hprof-analyzer heap report 33.hprof --section overview

# Walk retained size from a specific class:
hprof-analyzer heap query 34.hprof \
    --oql "SELECT @retainedSize, p.name \
           FROM _34_AppProductCatalog\$Product p \
           ORDER BY @retainedSize DESC LIMIT 10"
```

---

## Dump Method Reference

| Method | Triggers GC? | Captures unreachable? | When to use |
|--------|-------------|----------------------|-------------|
| `-XX:+HeapDumpOnOutOfMemoryError` | No (JVM captures at OOM) | No (JVM collects first) | OOM scenarios |
| `jcmd <pid> GC.heap_dump <file>` | **Yes** | No | Idle/live scenarios |
| `jcmd <pid> GC.heap_dump <file> -all=true` | Yes | **Yes** | When you want unreachable objects *and* jcmd |
| `jmap -dump:live,format=b,file=<file> <pid>` | **Yes** | No | Alternative to jcmd |
| `jmap -dump:format=b,file=<file> <pid>` | No | **Yes** | Scenario 30; comparative dumps |

---

## File Layout

```
leak-demos/
├── README.md                         ← this file
├── capture_all.sh                    ← batch capture script
│
├── 01_StaticCacheLeak.java           ┐
│   …                                  │  Parts 1–6 (scenarios 1–30)
├── 30_GcOverwhelmedGarbage.java      ┘
│
├── 31_AppWordFrequency.java          ┐
├── 32_AppSessionStore.java           │  Part 7: Normal applications
├── 33_AppEventPipeline.java          │
├── 34_AppProductCatalog.java         ┘
│
└── dumps/                            ← created by capture_all.sh
    ├── 01_StaticCacheLeak.hprof
    │   …
    ├── 34_AppProductCatalog.hprof
    └── logs/
        ├── 01_StaticCacheLeak.log
        └── …
```

---

## Adding Your Own Scenario

1. Create `NN_YourScenario.java` (use the next available number).
2. Choose a dump strategy:
   - **OOM**: just let it crash — `capture_all.sh` uses `HeapDumpOnOutOfMemoryError`.
   - **Idle/live**: print `HEAP_DUMP_READY` then `Thread.sleep(Long.MAX_VALUE)`.
   - **Unreachable objects**: print `DUMP WINDOW OPEN` at the right moment; script uses `jmap -dump:format=b`.
3. Add the corresponding `capture_live` / `capture_oom` / `capture_all_objects` call in `capture_all.sh`.
4. Update this README.

---

## Related Projects

- **hprof-analyzer**: <https://github.com/parttimenerd/hprof-analyzer>
- **SapMachine**: <https://github.com/SAP/SapMachine> (SAP's OpenJDK distribution)
- **cf-cli-java-plugin**: <https://github.com/SAP/cf-cli-java-plugin>

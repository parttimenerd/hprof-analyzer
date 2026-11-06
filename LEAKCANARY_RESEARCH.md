# LeakCanary / Shark Deep-Dive: Transferable Ideas for hprof-analyzer

> **Scope**: Extensive analysis of [LeakCanary](https://github.com/square/leakcanary) and its Shark heap analysis engine. Focus on concepts, algorithms, and design patterns that could significantly improve hprof-analyzer's analysis, reporting, or user experience for server-side JVM users.

---

## Executive Summary

LeakCanary's most impactful innovations are **not** in low-level HPROF parsing (hprof-analyzer already handles that well in Rust) but in **how it presents and contextualizes findings**. The key transferable ideas are:

1. **Leaking verdict propagation** — automatically narrowing the "suspect reference" in a retention path
2. **Object inspectors** — extensible per-type annotators that add domain-specific context
3. **Virtual references** — collapsing data structure internals into developer-readable edges
4. **Reference matcher catalog** — separating "known framework leaks" from "your code leaks"
5. **Leak signatures** — SHA-1 deduplication of leak reports across dumps
6. **Heap growth detection** — repeated-scenario reachability-tree diffing (BLeak-inspired)
7. **Prioritizing shortest path finder** — developer-readable paths over raw shortest paths

---

## 1. The "Leaking Verdict" System

### How It Works

Every object in a LeakCanary leak trace gets a verdict: `LEAKING: YES`, `LEAKING: NO`, or `LEAKING: UNKNOWN`. These are set by **object inspectors** (see §2) that examine each object's internal state.

The critical insight: the **suspect reference** is automatically identified as the edge between the last `NO` object and the first `YES` object. This narrows a potentially long retention path down to the one or two references that actually need fixing.

```
┬───
│ GC Root: Static field
│
├─ com.example.MySingleton class
│  Leaking: NO (a class is never leaking)
│  ↓ static MySingleton.leakedView
│  ~~~~~~~~~~~~~~~~~~~~~~~~~~~~       ← SUSPECT: this reference is the bug
├─ android.widget.TextView instance
│  Leaking: YES (View.mContext references a destroyed Activity)
│  mContext instance of com.example.MyActivity with mDestroyed = true
╰→
```

**The `~~~` underline marks the suspect reference** — the only thing the developer needs to look at.

**Recent rename (unreleased):** `Leaking: YES/NO/UNKNOWN` → `Verdict: Expected / Stuck / Unknown` — clearer terminology.

### Translation to hprof-analyzer

hprof-analyzer's leak suspects report already shows GC root paths, but every reference in the chain gets equal visual weight. Adding a verdict system would let the report **highlight the specific reference to fix**.

For server-side JVM, the verdicts would come from inspectors like:
- `java.lang.Class` → always `NOT_LEAKING` (classes outlive their instances)
- `java.lang.ClassLoader` → `NOT_LEAKING` unless it's a webapp classloader that should have been unloaded
- A Spring `ApplicationContext` that is `closed` → `LEAKING`
- A Hibernate `Session` that is `closed` → `LEAKING`
- An `ExecutorService` that is `shutdown` → `LEAKING`

**Impact**: Transforms the leak suspect report from "here's a chain of references" to "here's the one reference you need to fix."

**Sources:** [Fixing a Memory Leak docs](https://square.github.io/leakcanary/fundamentals-fixing-a-memory-leak/), [Design decisions](https://square.github.io/leakcanary/design/)

---

## 2. Object Inspector Plugin System

### Architecture

```kotlin
fun interface ObjectInspector {
  fun inspect(reporter: ObjectReporter)
}
```

`ObjectReporter` provides:
- `whenInstanceOf("com.example.MyClass") { instance -> ... }` — type-filtered callback
- `reporter.reportLeaking("reason")` / `reporter.reportNotLeaking("reason")` — set verdict
- `reporter.addLabel("someField = $value")` — attach contextual annotation

Inspectors are called for **every object** in a leak trace path. They add domain-specific context without the core engine needing to know about any particular framework.

### Built-in Android Inspectors

| Inspector | Object Type | "Leaking: YES" Condition |
|-----------|-------------|--------------------------|
| `ACTIVITY` | `android.app.Activity` | `mDestroyed = true` |
| `FRAGMENT` | AndroidX `Fragment` | `mFragmentManager = null` |
| `VIEW` | `android.view.View` | `mAttachInfo = null` and `mParent = null` |
| `VIEW_MODEL` | `androidx.lifecycle.ViewModel` | `mCleared = true` |
| `SERVICE` | `android.app.Service` | Object ID not in live service set |
| `WINDOW` | `android.view.Window` | `mDestroyed = true` |
| `APPLICATION` | `android.app.Application` | Always `NOT_LEAKING` (singleton) |
| `COMPOSITION_IMPL` | Jetpack Compose | `disposed = true` or state ≠ `RUNNING` |

### Server-Side JVM Translation

The same pattern maps directly to server-side frameworks:

| Framework Concept | Inspector Logic |
|---|---|
| **Spring `ApplicationContext`** | If `isActive() == false` → `LEAKING` |
| **Spring `@RequestScope` bean** | If held outside request context → suspect |
| **Hibernate `Session` / `EntityManager`** | If `isOpen() == false` → `LEAKING` |
| **Hibernate entity** | If `DETACHED` and held externally → suspect |
| **Netty `Channel`** | If `isActive() == false` → `LEAKING` |
| **`ExecutorService`** | If `isShutdown() == true` → `LEAKING` |
| **`java.sql.Connection`** | If `isClosed() == true` → `LEAKING` |
| **`HttpSession`** | If `invalidated` → `LEAKING` |
| **`java.lang.Thread`** | If `state == TERMINATED` and still held → `LEAKING` |
| **`ClassLoader` (webapp)** | If webapp undeployed but loader still referenced → `LEAKING` |

**Implementation in hprof-analyzer:** These could be implemented as:
- A built-in set of JVM inspectors (Class, ClassLoader, Thread, Connection, etc.)
- An optional set of framework inspectors (Spring, Hibernate, Netty)
- A user-extensible TOML/config file for custom inspectors

**Impact**: Leak traces become self-interpreting. Instead of "here's a chain, figure it out", the report says "this Session is closed but still referenced via this field."

**Sources:** [Code Recipes](https://square.github.io/leakcanary/recipes/), [AndroidObjectInspectors.kt](https://github.com/square/leakcanary/blob/main/shark/shark-android/src/main/java/shark/AndroidObjectInspectors.kt)

---

## 3. Virtual References (ReferenceReader / Expanders)

### The Problem

Raw heap dump reference paths expose data structure internals that obscure the logical relationship:

**Raw path:** `cache.table[42].next.next.key → MyObject`
**What the developer wrote:** `cache.get("myKey")` → `MyObject`

### LeakCanary's Solution

`ReferenceReader` implementations (called "expanders" since 3.0-alpha-8) define custom traversal for data structures. They emit **virtual references** — edges that don't correspond to a single heap field but represent logical relationships.

Examples:
- **HashMap/LinkedHashMap:** Collapse bucket chain (`table[N].next.next.key`) into `map["keyValue"]`
- **ArrayList:** Collapse `elementData[N]` into `list[N]`
- **WeakReference:** The `ANIMATOR_WEAK_REF_SUCKS` expander follows `WeakReference.referent` and presents the actual target, hiding the weak reference wrapper
- **SafeIterableMap:** Custom traversal for Android's `LiveData` observer maps

`ReferenceReader` became a **public API** in 3.0-alpha-8 (June 2024), so library authors can define custom graph traversal for their own data structures.

### Translation to hprof-analyzer

hprof-analyzer already understands some collection internals for the collections report. Extending this to **leak trace presentation** would make GC root paths dramatically more readable:

| Data Structure | Raw Path | Virtual Reference |
|---|---|---|
| `HashMap` | `table[42].next.key` | `map["keyValue"]` |
| `ArrayList` | `elementData[3]` | `list[3]` |
| `ConcurrentHashMap` | `table[17].next.val` | `concurrentMap["key"]` |
| `ThreadLocal` | `threadLocals.table[5].value` | `threadLocal<MyClass>` |
| `WeakReference` | `referent` | follows through to actual target |
| `Optional` | `value` | follows through to contained value |

**Impact**: GC root paths in leak suspects become readable as code, not as JVM internals.

**Sources:** [Design decisions](https://square.github.io/leakcanary/design/), [FAQ](https://github.com/square/leakcanary/blob/main/docs/faq.md), [Changelog](https://square.github.io/leakcanary/changelog/)

---

## 4. Reference Matcher Catalog (Known Framework Leaks)

### Architecture

`AndroidReferenceMatchers` is an enum where each entry defines one or more `ReferenceMatcher` instances:

```kotlin
enum class AndroidReferenceMatchers {
  ACTIVITY_THREAD__M_NEW_ACTIVITIES {
    override fun add(references: MutableList<ReferenceMatcher>) {
      references += instanceFieldLeak("android.app.ActivityThread", "mNewActivities") {
        sdkInt in 19..27  // Only applies to API 19-27
      }
    }
  },
  // ... dozens more
}
```

Three pattern types:
- `StaticFieldPattern(className, fieldName)` — static field leak
- `InstanceFieldPattern(className, fieldName)` — instance field leak
- `JavaLocalPattern(threadName)` — local variable on a specific thread

Each matcher can be gated by SDK version, manufacturer, model — `patternApplies: AndroidBuildMirror.() -> Boolean`.

Two matcher modes:
- **`IgnoredReferenceMatcher`** — reference is entirely excluded from path finding (never traversed)
- **`LibraryLeakReferenceMatcher`** — reference is deprioritized in path finding and leak is categorized as "Library Leak" (vs "Application Leak") in the report

### Translation to hprof-analyzer

A **known JVM/framework leak catalog** would separate "your code's fault" from "known framework bug":

| Pattern | Type | Description |
|---|---|---|
| `java.lang.ref.Finalizer.unfinalized` | Ignored | Not a leak — JVM internal bookkeeping |
| `sun.misc.Unsafe.theUnsafe` | Ignored | JVM internal singleton |
| `java.util.logging.Level.known` | Library | Known JDK logging framework retention |
| `org.apache.tomcat.util.threads.TaskThread` | Library (version-gated) | Tomcat thread pool leak (fixed in 9.0.x) |
| `com.mysql.cj.jdbc.AbandonedConnectionCleanupThread` | Library | MySQL JDBC driver leak (classloader) |
| `org.springframework.boot.devtools.restart.RestartClassLoader` | Library | Spring DevTools hot reload leak |
| `ch.qos.logback.classic.Logger.parent` | Library | Logback logger tree retention |

**Report impact:** Leak suspects would be split into:
- **Application Leaks** — bugs in your code, highlighted prominently
- **Library Leaks** — known framework issues, reported separately with workaround notes
- **Ignored References** — excluded from path finding entirely

**Implementation:** A TOML file shipped with hprof-analyzer, user-extensible:
```toml
[[known_leaks]]
class = "com.mysql.cj.jdbc.AbandonedConnectionCleanupThread"
field = "connectionFinalizerPhantomRefs"
type = "library"
description = "MySQL JDBC driver classloader leak — switch to HikariCP connection pool"
versions = "8.0.0-8.0.28"
```

**Sources:** [AndroidReferenceMatchers.kt](https://github.com/square/leakcanary/blob/main/shark/shark-android/src/main/java/shark/AndroidReferenceMatchers.kt), [How LeakCanary works](https://square.github.io/leakcanary/fundamentals-how-leakcanary-works/)

---

## 5. Leak Signature for Deduplication

### Algorithm

The **leak signature** is a SHA-1 hash of the concatenated suspect reference identifiers:

```
signature = SHA1(
  "com.example.MySingleton.leakedView" +
  "java.util.ArrayList.elementData" +
  "java.lang.Object[].[3]"
)
// → "dbfa277d7e5624792e8b60bc950cd164190a11aa"
```

Only the **suspect references** (those between the last `NOT_LEAKING` and first `LEAKING` objects) are included. References above and below the suspect zone are excluded.

### Why This Matters

- **Same bug = same signature** across different heap dumps, different runs, different users
- Enables grouping in crash trackers (e.g., Bugsnag `event.groupingHash = leak.signature`)
- Enables tracking whether a fix eliminated a specific leak
- Enables counting distinct bugs vs. distinct occurrences

### Translation to hprof-analyzer

For `hprof-analyzer compare reports`, leak signatures would enable:
- **Tracking specific leaks across dumps**: "Leak `dbfa27` was present in dump A and dump B — it's persistent"
- **Deduplicating leak suspects**: "3 instances of the same leak pattern (signature `dbfa27`)"
- **CI regression detection**: "New leak signature `abc123` appeared after this commit"
- **Machine-readable output**: The signature is stable, short, and diffable

**Sources:** [How LeakCanary works](https://square.github.io/leakcanary/fundamentals-how-leakcanary-works/), [Changelog 2.1](https://square.github.io/leakcanary/changelog/)

---

## 6. Heap Growth Detection (BLeak-Inspired)

### The Problem It Solves

Classic leak detection (via `ObjectWatcher`) catches objects that should have been collected but weren't. It **doesn't** catch:
- Caches that gain an entry on every request but never evict
- Listener lists that grow without bound
- Log buffers that accumulate indefinitely
- ThreadLocal values that accumulate across pooled thread reuse

These are "growth" problems, not "leak" problems — the objects are intentionally held, just for too long.

### Algorithm: `RepeatingScenarioObjectGrowthDetector`

```
findRepeatedlyGrowingObjects(scenario, maxHeapDumps, scenarioLoopsPerDump=2):
  for i in 1..maxHeapDumps:
    run scenario() × scenarioLoopsPerDump times
    trigger GC
    dump heap
    traverse from GC roots → build shortest-path tree (object count per node)
    diff this tree vs. previous tree
    keep only nodes where count grew by ≥ scenarioLoopsPerDump
    if no growing nodes OR i == maxHeapDumps: return results
```

Key design decisions:
- **Diffs reachability trees, not class histograms** — isolates *specific object subgraphs* that are growing, not just "more Strings appeared"
- **Requires growth ≥ scenarioLoopsPerDump per dump** — filters one-time allocations and GC noise
- **"Repeating scenario" concept** — the scenario is a user action (e.g., handle request). Running it multiple times amplifies the leak signal while keeping noise constant
- **Iterative convergence** — keeps dumping until no new growth is detected or max dumps reached

### Translation to hprof-analyzer

hprof-analyzer already has `compare reports` for diffing two dumps. The BLeak-style approach could enhance this:

1. **`hprof-analyzer compare growth dump1.json dump2.json dump3.json`** — ingest a series of report JSONs and identify **consistently growing subgraphs** (not just growing classes)
2. **Growth annotation in compare output** — mark classes as `spike` (sudden), `trend` (consistent growth), or `stable`
3. **Reachability-tree diffing** — show not just "N more HashMap instances" but "N more HashMap instances reachable from `com.example.CacheManager.cache`"

**Sources:** [heap-growth.md](https://github.com/square/leakcanary/blob/main/docs/heap-growth.md), [RepeatingScenarioObjectGrowthDetector.kt](https://github.com/square/leakcanary/blob/0f9b924b/shark/shark/src/main/java/shark/RepeatingScenarioObjectGrowthDetector.kt)

---

## 7. Prioritizing Shortest Path Finder

### The Problem

Standard BFS from GC roots finds the **shortest** path to a retained object. But the shortest path often goes through JVM internals or framework plumbing that the developer doesn't control:

```
// Shortest path (unhelpful):
GC Root → Thread → threadLocals → ThreadLocalMap → Entry → value → MyObject

// Prioritized path (actionable):
GC Root → MySingleton.cache → HashMap → MyObject
```

### LeakCanary's Solution: Two-Phase BFS

1. **Phase 1:** BFS from all GC roots. References classified as "visit last" (library leak references, thread locals, JVM internals) go to the **back** of the deque instead of the front.
2. **Phase 2:** If targets aren't found via Phase 1, drain the "visit last" queue.

Result: the **shortest path among developer-readable references**, falling back to framework references only when necessary.

### Translation to hprof-analyzer

The leak suspects report currently shows whatever path the dominator tree / BFS finds first. A prioritized path finder would:
- **Prefer app-code references** (user packages) over JDK/framework references
- **Deprioritize thread locals, JNI globals, monitor roots** unless no other path exists
- **Label each path node** with whether it's app code, framework, or JVM internal

This is particularly valuable for the `--section leaks` output where users need to know what **they** should fix.

**Sources:** [PrioritizingShortestPathFinder.kt](https://github.com/square/leakcanary/blob/0f9b924b/shark/shark/src/main/java/shark/PrioritizingShortestPathFinder.kt), [Design decisions](https://square.github.io/leakcanary/design/)

---

## 8. Design Philosophy Lessons

### "Single Bad Reference" Model

LeakCanary's foundational principle: **a leak is always a single bad reference** — one field or variable that holds an object it should have released. Everything below that reference is stuck only because of that one mistake.

This shapes everything:
- Report the **shortest** path (fewer candidates to examine)
- **Highlight** only the suspect reference(s), not the entire chain
- Use the verdict system to **narrow** which reference is suspect

**Translation:** hprof-analyzer's triage signals could adopt this framing: "The fix is to remove/clear **this one reference**" rather than describing the entire retention chain.

### Conservative False Positive Avoidance

LeakCanary uses multiple layers:
1. **5-second delay** before flagging retention (filters transient post-lifecycle holds)
2. **Library leak separation** (framework bugs don't appear as app bugs)
3. **Conservative inspectors** (only fire when "reasonably sure")
4. **Incremental reporting** (already-reported leaks don't resurface)

**Translation:** hprof-analyzer's triage signals could add confidence levels: `HIGH CONFIDENCE: Single object retains 78% of heap via static field` vs `LOW CONFIDENCE: Thread retention — may be normal for thread pools`.

### Making Heap Analysis Accessible to Non-Experts

Key LeakCanary accessibility decisions:
- **"The report comes to you"** — notification-based, zero setup
- **Shortest-path preference** — minimize what the developer must read
- **Verdict propagation** — automatically narrow the suspect
- **Virtual references** — traces match how developers think, not JVM internals
- **Labels** — contextual annotations explain *why* each verdict was given

**Translation:** hprof-analyzer's reports could include:
- A **plain-English summary** for each leak suspect ("A static `HashMap` in `CacheManager` holds 150MB of `byte[]` arrays. The fix: add eviction or clear the map.")
- **Suspect highlighting** in the GC root path (not all references equal)
- **"Why is this alive?"** annotations on each node

**Sources:** [Design decisions](https://square.github.io/leakcanary/design/), [History](https://square.github.io/leakcanary/history/), [The LeakCanary Method blog (Oct 2025)](https://engineering.block.xyz/blog/the-leakcanary-method)

---

## 9. Additional Notable Technical Details

### Shark's Memory Efficiency

- **SortedBytesMap**: Single flat `ByteArray` with N-byte entries, binary-searchable. Zero Java object wrapper overhead. A 160MB dump that needs 2GB in Android Studio opens in **40MB** with Shark.
- **PagedSortedBytesMap** (PR #2835, 2026): Splits storage across power-of-two pages for indexes > 2GB. Verified on **27GB JVM heap dumps**.
- **Bitset visited-set** (unreleased): 1 bit per object index → **509KB** vs 100.7MB for a 4M-object heap traversal.
- **Compact path representation**: Parent stored as `(parentObjectIndex, referenceIndex)` pair → **37MB** vs much larger for 2.3M nodes.
- **Thread-safe HeapGraph** (unreleased): Concurrent reads enabled for multi-threaded analysis.

### shark-cli Commands

| Command | Description |
|---------|-------------|
| `analyze` | Full leak analysis on an HPROF file |
| `strip-hprof` | Zero primitive arrays for safe sharing (like `hprof-analyzer redact`) |
| `interactive` | REPL for exploring heap objects |
| `neo4j` | Convert HPROF to Neo4j graph database (experimental) |
| `heap-growth` | Analyze directory of HProfs for growing objects |
| `dump-process` | Trigger dump from connected Android device |

### shark-dive: Dominator Tree Treemap

A **desktop Compose application** that visualizes a heap dump as a treemap of the dominator tree. Each block = an object, area = retained size, nesting = domination chain. Uses the exact Lengauer-Tarjan dominator tree (not the approximate on-device version).

### "Also Retains" Multi-Leak Accountability

When multiple retained objects exist and some are only reachable through another leaking object, LeakCanary reports the outermost leak's trace but annotates it: "Also retains com.example.Foo instance 0x12c0a6b8". This ensures every retained object is accounted for without redundant traces.

**Translation:** hprof-analyzer's leak suspects could annotate: "Suspect 1 also retains Suspect 3 — fixing Suspect 1 would free both."

### Plumber Module: Automatic Runtime Fixes

`plumber-android` applies automatic runtime workarounds for known AOSP/vendor leaks:
- Clearing static pools on `onTrimMemory()`
- Dispatching `IdleHandler`s to clear leaky references
- Reflection-based cleanup for ROM-specific leaks

**No direct hprof-analyzer translation** (hprof-analyzer is an analysis tool, not a runtime), but the **catalog of known leaks and their workarounds** could be included as advice in leak suspect reports.

---

## Summary: Priority-Ranked Transferable Ideas

| # | Concept | Effort | Impact | Notes |
|---|---------|--------|--------|-------|
| 1 | **Leak verdict system** (YES/NO/UNKNOWN per node) | Medium | Very High | Transforms leak reports from "here's a chain" to "fix this reference" |
| 2 | **Object inspectors** (framework-aware annotations) | Medium | Very High | Spring/Hibernate/Netty inspectors would cover most server-side users |
| 3 | **Virtual references** (collection internals collapsed) | Medium | High | Makes GC root paths readable as code |
| 4 | **Known leak catalog** (app vs library leak separation) | Low | High | TOML file of known JDK/framework leak patterns |
| 5 | **Prioritizing path finder** (prefer app-code paths) | Medium | High | More actionable leak traces |
| 6 | **Leak signatures** (SHA-1 dedup) | Low | Medium | Enables CI regression detection and cross-dump tracking |
| 7 | **"Also retains" annotations** | Low | Medium | Shows which leaks are dependent on each other |
| 8 | **Heap growth detection** (reachability-tree diffing) | High | High | Catches unbounded caches/lists not caught by standard leak detection |
| 9 | **Plain-English leak summaries** | Low | High | "The fix: add eviction to this static HashMap" |
| 10 | **Confidence levels on triage signals** | Low | Medium | HIGH/MEDIUM/LOW confidence reduces false-positive noise |

---

## Key Sources

- [LeakCanary Design Decisions](https://square.github.io/leakcanary/design/)
- [How LeakCanary Works](https://square.github.io/leakcanary/fundamentals-how-leakcanary-works/)
- [Fixing a Memory Leak](https://square.github.io/leakcanary/fundamentals-fixing-a-memory-leak/)
- [Shark Architecture](https://square.github.io/leakcanary/shark/)
- [Announcing Shark (blog)](https://developer.squareup.com/blog/announcing-shark-smart-heap-analysis-reports-for-kotlin/)
- [The LeakCanary Method (Block Engineering, Oct 2025)](https://engineering.block.xyz/blog/the-leakcanary-method)
- [Heap Growth Detection](https://github.com/square/leakcanary/blob/main/docs/heap-growth.md)
- [Code Recipes](https://square.github.io/leakcanary/recipes/)
- [LeakCanary History](https://square.github.io/leakcanary/history/)
- [Changelog](https://square.github.io/leakcanary/changelog/)
- [PrioritizingShortestPathFinder.kt](https://github.com/square/leakcanary/blob/0f9b924b/shark/shark/src/main/java/shark/PrioritizingShortestPathFinder.kt)
- [RealLeakTracerFactory.kt](https://github.com/square/leakcanary/blob/b5e059c8f8323f1df5f43127e8a55d9f3cdf7dd4/shark/shark/src/main/java/shark/RealLeakTracerFactory.kt)
- [AndroidObjectInspectors.kt](https://github.com/square/leakcanary/blob/main/shark/shark-android/src/main/java/shark/AndroidObjectInspectors.kt)
- [AndroidReferenceMatchers.kt](https://github.com/square/leakcanary/blob/main/shark/shark-android/src/main/java/shark/AndroidReferenceMatchers.kt)
- [DeepWiki: Shark Heap Analysis Engine](https://deepwiki.com/square/leakcanary/3.3-shark:-heap-analysis-engine)
- [PR #2835: PagedSortedBytesMap](https://github.com/square/leakcanary/pull/2835)

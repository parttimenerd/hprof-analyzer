# Heap Dump Analysis: `dump_2_scala-doku.hprof`


_All sizes are binary (1 KB = 1024 bytes, 1 MB = 1024 KB, and so on)._

----

## Contents

- [Summary](#summary)
- [Memory Triage](#memory-triage)
- [Waste Summary](#waste-summary)
- [System Overview](#system-overview)
- [Leak Suspects](#leak-suspects)
- [Top Consumers](#top-consumers)
- [Dominator Analysis](#dominator-analysis)
- [Threads](#threads)
- [Top Components](#top-components)
- [Arrays by Size](#arrays-by-size)
- [Collections](#collections)
- [Collection Waste Budget](#collection-waste-budget)
- [Top Retainers](#top-retainers)
- [References](#references)
- [Unreachable Objects](#unreachable-objects)
- [Allocation Sites](#allocation-sites)
- [Retention Concentration](#retention-concentration)
- [Dominator-Depth Distribution](#dominator-depth-distribution)
- [Glossary](#glossary)

----

## Summary

_At-a-glance digest; see the sections below for full detail._

| Metric               |   Value |
| -------------------- | ------: |
| Total reachable heap | 29.8 MB |
| Objects              | 952,666 |
| Classes              |   2,851 |
| Class loaders        |       5 |
| Threads              |       4 |
| GC roots             |   1,509 |

**Top suspects by retained heap**

|  # | Suspect                             | Retained | % Heap |
| -: | ----------------------------------- | -------: | -----: |
|  1 | `java.lang.Thread` (single object)  |  22.9 MB |  76.7% |
|  2 | `java.lang.Class` (1,669 instances) |   3.5 MB |  11.7% |

**Likely problem:** `java.lang.Thread` retains 76.7% of the reachable heap — investigate this first.

## Memory Triage

_Automated signals pointing to where memory concentrates and what to investigate first. Total reachable heap: 29.8 MB_

- **Headline Retainer:** `java.lang.Thread` (a single object) retains 22.9 MB (76.7% of reachable heap). See [Leak Suspects](#leak-suspects).
- **Concentration:** highly concentrated — `java.lang.Thread` (a single object) holds 76.7% of the heap; making it unreachable would reclaim most memory. See [Leak Suspects](#leak-suspects).
- **Dominant GC-Root Type:** 76.7% of the heap is held by "Thread" roots — retention concentrates at one root type; investigate why this root category holds so much. See [System Overview](#system-overview).
- **Shape:** deep — long dominator chains suggest nested collections or linked structures; trace the chain to find the retaining root — 90% of objects within depth 11273, max depth 41355. See [Dominator-Depth Distribution](#dominator-depth-distribution).
- **One Leak or Many:** the single biggest object, `java.lang.Thread`, retains 76.7% and the top 10 retain 92.6% of the heap; 4 objects each hold ≥1%. See [Top Consumers](#top-consumers).
- **Class-Loader Reload (Low Count):** `scala.collection.immutable.$colon$colon` is loaded by 2 class loaders (8.6 MB retained) — possible reload, but count is low; investigate only if count grows. See [Duplicate Classes](#system-overview).
- **Thread Pinning:** thread `main` retains 22.9 MB (76.7% of heap) and pins 124 thread-local roots — a live thread is holding a disproportionate amount of memory alive. Inspect the thread's stack frames and ThreadLocal values in the Threads section. See [Threads](#threads).
- **Off-Heap (DirectByteBuffer):** 134.3 MB of native memory is held by live DirectByteBuffers — not reflected in the on-heap totals, but counts against process RSS and can trigger OS-level OOM. See [Off-Heap NIO](#off-heap-nio).
- **Sparse Object Arrays:** 38,119 object arrays are <=20% full (5.9 MB wasted on null slots) — sparse or multi-dimensional array structures consuming excess memory. Replace with a `HashMap`/`SparseArray`, a `List` that grows on demand, or a dedicated sparse-matrix library. See [Collections](#collections).
- **Fixed per-Object Header Overhead:** 10.9 MB (36.6% of heap) consumed by JVM object headers alone (952,666 objects × 12 B each) — consider replacing wrapper objects with primitive arrays, off-heap buffers, or primitive-specialized collections. See [Object Header Overhead](#object-header-overhead).
- **Empty-Collection Cemetery:** 5,806 of 6,307 tracked collections (92.1%) are empty — pre-allocated but never populated containers waste object-header overhead. Consider lazy initialization, returning `Collections.emptyList()` sentinels, or using `null` until the collection is first written. See [Collections](#collections).
- **Collection Waste Not Analyzed:** _Collection waste not analyzed — re-run with `--collections` to check for wasted capacity._

## Waste Summary

_Approximately **6.3 MB** estimated reclaimable across the sources below — duplicate strings, duplicate primitive arrays, boxed primitives, and empty/singleton collection overhead. Fix the biggest category first for the highest impact. Figures are approximate; sources may overlap._

| Source                                     | Reclaimable |
| ------------------------------------------ | ----------: |
| [Under-filled Object Arrays](#collections) |      6.1 MB |
| [Under-filled Collections](#collections)   |    175.8 KB |

## System Overview

_JVM and dump metadata, heap totals, GC root breakdown, class loader sizes, and system properties._

### Heap Summary

| Property                                      | Value                            |
| --------------------------------------------- | -------------------------------- |
| HPROF format                                  | JAVA PROFILE 1.0.2               |
| File size                                     | 50.5 MB                          |
| Identifier size                               | 64-bit                           |
| Compressed OOPs                               | yes                              |
| Dump created                                  | 2026-07-08T12:44:31Z             |
| Total objects                                 | 952,666                          |
| Total reachable heap                          | 29.8 MB                          |
| Off-heap / on-heap                            | 134.3 MB off-heap (4.5× on-heap) |
| GC roots                                      | 1,509                            |
| Classes loaded                                | 2,851                            |
| Class loaders                                 | 5                                |
| Unreachable objects (excluded)                | 4,266 (673.0 KB)                 |
| Heap fragmentation (unreachable / heap total) | 2.2%                             |
| Top-class retained concentration              | 76.7%                            |

- **Class loaders (labels):** java/net/URLClassLoader, jdk/internal/loader/ClassLoaders$AppClassLoader, jdk/internal/loader/ClassLoaders$PlatformClassLoader

### GC Roots by Type

_GC roots are the entry points where the JVM starts reachability scanning — anything reachable from a root stays alive. Common root types: thread-stack locals, JNI global references, static fields of loaded classes, and synchronized lock objects._

| Root Type    | Count |                  |
| ------------ | ----: | ---------------- |
| Sticky Class | 1,402 | ████████████████ |
| JNI Global   |   100 | █▏               |
| Thread       |     7 | ▏                |

### Heap Composition

_Shallow heap broken down by object kind: instances, object arrays, primitive arrays, and class objects._

| Kind             | Objects | Shallow Heap |                  |
| ---------------- | ------: | -----------: | ---------------- |
| Instances        | 770,497 |      19.7 MB | ████████████████ |
| Object Arrays    |  65,061 |       5.4 MB | ████▍            |
| Primitive Arrays | 114,257 |       4.7 MB | ███▊             |
| Class Objects    |   2,851 |      34.9 KB | ▏                |

### HPROF Record Census

_Raw HPROF record-type composition of the dump (pass-1 counts). Useful for diagnosing truncated or unusual dumps (e.g. zero stack frames means no allocation-site data; a mismatch between load-class and class-dump counts can indicate a partial write). Additive, not parity-compared._

| Record Type           |   Count |
| --------------------- | ------: |
| UTF8 strings          |  63,756 |
| Load class            |   2,967 |
| Unload class          |       0 |
| Stack frames          |      57 |
| Stack traces          |       8 |
| Heap dump segments    |      49 |
| Instance dumps        | 771,860 |
| Object-array dumps    |  65,121 |
| Primitive-array dumps | 116,983 |
| Class dumps           |   2,967 |

#### GC Root Records by Tag

| Root Tag     | Count |
| ------------ | ----: |
| Sticky Class | 1,406 |
| Java Frame   |   151 |
| JNI Global   |   100 |
| Thread       |     7 |

### Duplicate Strings (approximate)

_Duplicate-string analysis not run (pass `--find-duplicates`)._

### Duplicate Primitive Arrays (approximate)

_Duplicate primitive-array analysis not run (pass `--find-duplicates`)._

### Boxed Numbers

_Heap consumed by `Integer`, `Long`, `Double`, and other boxed wrapper types. Each boxed value costs 16–24 bytes (12-byte object header + primitive field, padded to 8-byte boundary) versus 4–8 bytes for an unboxed primitive. Replacing with primitive fields or `int[]`/`long[]` arrays eliminates the per-object header._

|  # | Class                 | Instances | Total Shallow | % of Heap | Avg Size |
| -: | --------------------- | --------: | ------------: | --------: | -------: |
|  1 | `java.lang.Integer`   |     9,789 |      153.0 KB |      0.5% |     16 B |
|  2 | `java.lang.Long`      |       256 |        6.0 KB |     <0.1% |     24 B |
|  3 | `java.lang.Byte`      |       256 |        4.0 KB |     <0.1% |     16 B |
|  4 | `java.lang.Short`     |       256 |        4.0 KB |     <0.1% |     16 B |
|  5 | `java.lang.Character` |       128 |        2.0 KB |      0.0% |     16 B |
|  6 | `java.lang.Boolean`   |         2 |          32 B |      0.0% |     16 B |
|  7 | `java.lang.Double`    |         1 |          24 B |      0.0% |     24 B |
|  8 | `java.lang.Float`     |         1 |          16 B |      0.0% |     16 B |

### Object Header Overhead

_Classes where object headers (12 bytes with compressed OOPs, 16 without) consume a large share of shallow heap. The practical action is to reduce object *count*: merge small objects, use primitive arrays instead of boxed wrappers, or replace fine-grained instances with a flat array of fields. Value types (Project Valhalla) eliminate headers entirely._

|  # | Class                                             | Instances | Hdr/obj | Total Headers | Hdr % | Avg Size |
| -: | ------------------------------------------------- | --------: | ------: | ------------: | ----: | -------: |
|  1 | `scala.collection.immutable.$colon$colon`         |   146,151 |    12 B |        1.7 MB | 50.0% |     24 B |
|  2 | `java.lang.Object`                                |   133,780 |    12 B |        1.5 MB | 75.0% |     16 B |
|  3 | `cafesat.sat.Literal`                             |   125,219 |    12 B |        1.4 MB | 30.0% |     40 B |
|  4 | `int[]`                                           |    89,265 |    12 B |        1.0 MB | 38.7% |     30 B |
|  5 | `cafesat.sat.Solver$Clause`                       |    65,565 |    12 B |      768.3 KB | 37.5% |     32 B |
|  6 | `scala.collection.immutable.Set$Set2`             |    44,628 |    12 B |      523.0 KB | 50.0% |     24 B |
|  7 | `cafesat.asts.core.Trees$ConnectiveApplication`   |    43,982 |    12 B |      515.4 KB | 50.0% |     24 B |
|  8 | `cafesat.sat.Vector`                              |    36,614 |    12 B |      429.1 KB | 50.0% |     24 B |
|  9 | `cafesat.asts.core.Trees$ConnectiveSymbol`        |    35,234 |    12 B |      412.9 KB | 50.0% |     24 B |
| 10 | `java.lang.String`                                |    23,997 |    12 B |      281.2 KB | 50.0% |     24 B |
| 11 | `scala.collection.immutable.BitmapIndexedSetNode` |    22,791 |    12 B |      267.1 KB | 30.0% |     40 B |
| 12 | `cafesat.asts.core.Trees$PredicateApplication`    |    19,761 |    12 B |      231.6 KB | 50.0% |     24 B |
| 13 | `java.util.HashMap$Node`                          |    10,167 |    12 B |      119.1 KB | 37.5% |     32 B |
| 14 | `java.lang.Integer`                               |     9,789 |    12 B |      114.7 KB | 75.0% |     16 B |
| 15 | `cafesat.api.Formulas$Formula`                    |     8,908 |    12 B |      104.4 KB | 75.0% |     16 B |
| 16 | `scala.collection.immutable.Set$Set3`             |     8,748 |    12 B |      102.5 KB | 50.0% |     24 B |
| 17 | `java.util.concurrent.ConcurrentHashMap$Node`     |     7,007 |    12 B |       82.1 KB | 37.5% |     32 B |
| 18 | `java.util.jar.Attributes`                        |     5,786 |    12 B |       67.8 KB | 75.0% |     16 B |
| 19 | `java.lang.Class`                                 |     2,860 |    12 B |       33.5 KB | 94.3% |     12 B |
| 20 | `java.util.LinkedHashMap$Entry`                   |     1,200 |    12 B |       14.1 KB | 30.0% |     40 B |
| 21 | `java.lang.invoke.MemberName`                     |     1,040 |    12 B |       12.2 KB | 30.0% |     40 B |
| 22 | `jdk.internal.util.WeakReferenceKey`              |       900 |    12 B |       10.5 KB | 37.5% |     32 B |
| 23 | `java.lang.invoke.MethodType`                     |       894 |    12 B |       10.5 KB | 30.0% |     40 B |
| 24 | `java.lang.invoke.ResolvedMethodName`             |       803 |    12 B |        9.4 KB | 75.0% |     16 B |
| 25 | `cafesat.asts.core.Trees$PredicateSymbol`         |       729 |    12 B |        8.5 KB | 50.0% |     24 B |
| 26 | `cafesat.api.Formulas$PropVar`                    |       729 |    12 B |        8.5 KB | 75.0% |     16 B |
| 27 | `java.lang.Class[]`                               |       725 |    12 B |        8.5 KB | 38.4% |     31 B |
| 28 | `java.lang.invoke.LambdaForm$Name`                |       503 |    12 B |        5.9 KB | 37.5% |     32 B |
| 29 | `java.lang.String[]`                              |       449 |    12 B |        5.3 KB | 30.9% |     38 B |
| 30 | `java.lang.module.ModuleDescriptor$Exports`       |       367 |    12 B |        4.3 KB | 50.0% |     24 B |

### Class Histogram (by Retained Heap)

_Every loaded class with its instance count, shallow heap (own bytes), and retained heap (what would be reclaimed if all instances became unreachable). Top 50 shown; full list in JSON._

|  # | Class                                             | Instances | Shallow Heap |  Largest | Retained Heap | % Heap |
| -: | ------------------------------------------------- | --------: | -----------: | -------: | ------------: | -----: |
|  1 | `java.lang.Thread`                                |        27 |       2.7 KB |    104 B |       22.9 MB |  76.7% |
|  2 | `java.lang.Object[]`                              |    25,579 |       1.5 MB | 512.0 KB |       11.4 MB |  38.4% |
|  3 | `scala.collection.immutable.$colon$colon`         |   146,151 |       3.3 MB |     24 B |        8.6 MB |  28.8% |
|  4 | `scala.collection.immutable.BitmapIndexedSetNode` |    22,791 |     890.3 KB |     40 B |        8.4 MB |  28.1% |
|  5 | `scala.collection.immutable.HashSet`              |        85 |       1.3 KB |     16 B |        8.0 MB |  26.9% |
|  6 | `cafesat.sat.Literal`                             |   125,219 |       4.8 MB |     40 B |        4.8 MB |  16.0% |
|  7 | `cafesat.sat.Solver`                              |         1 |        168 B |    168 B |        4.6 MB |  15.6% |
|  8 | `scala.collection.immutable.Set$Set2`             |    44,628 |       1.0 MB |     24 B |        4.4 MB |  14.9% |
|  9 | `cafesat.sat.Vector[]`                            |         1 |     143.0 KB | 143.0 KB |        4.3 MB |  14.5% |
| 10 | `cafesat.sat.Vector`                              |    36,614 |     858.1 KB |     24 B |        4.2 MB |  14.1% |
| 11 | `cafesat.asts.core.Trees$ConnectiveApplication`   |    43,982 |       1.0 MB |     24 B |        3.9 MB |  13.0% |
| 12 | `cafesat.sat.Solver$Clause`                       |    65,565 |       2.0 MB |     32 B |        3.6 MB |  12.1% |
| 13 | `java.lang.Class`                                 |     2,860 |      35.5 KB |   1.1 KB |        3.5 MB |  11.9% |
| 14 | `cafesat.sat.Solver$Clause[]`                     |    36,615 |       3.4 MB |  71.5 KB |        3.4 MB |  11.5% |
| 15 | `int[]`                                           |    89,265 |       2.6 MB |  71.5 KB |        2.6 MB |   8.8% |
| 16 | `scala.runtime.LazyVals$`                         |         1 |         16 B |     16 B |        2.5 MB |   8.4% |
| 17 | `java.lang.Object`                                |   133,780 |       2.0 MB |     16 B |        2.0 MB |   6.8% |
| 18 | `byte[]`                                          |    24,721 |       2.0 MB | 255.1 KB |        2.0 MB |   6.7% |
| 19 | `java.lang.String`                                |    23,997 |     562.4 KB |     24 B |        1.7 MB |   5.6% |
| 20 | `java.util.HashMap`                               |       361 |      16.9 KB |     48 B |        1.6 MB |   5.4% |
| 21 | `java.util.HashMap$Node[]`                        |       395 |      96.5 KB |  16.0 KB |        1.6 MB |   5.3% |
| 22 | `java.util.HashMap$Node`                          |    10,167 |     317.7 KB |     32 B |        1.5 MB |   5.1% |
| 23 | `scala.collection.immutable.Set$Set3`             |     8,748 |     205.0 KB |     24 B |        1.2 MB |   4.0% |
| 24 | `java.lang.ref.SoftReference`                     |       210 |       8.2 KB |     40 B |        1.2 MB |   3.9% |
| 25 | `java.util.jar.JarFile`                           |        10 |        640 B |     64 B |        1.2 MB |   3.9% |
| 26 | `java.util.jar.Manifest`                          |         8 |        192 B |     24 B |        1.1 MB |   3.8% |
| 27 | `scala.runtime.ObjectRef`                         |         1 |         16 B |     16 B |      962.2 KB |   3.2% |
| 28 | `java.util.zip.ZipFile$Source`                    |        10 |        800 B |     80 B |      828.1 KB |   2.7% |
| 29 | `cafesat.asts.core.Trees$ConnectiveSymbol`        |    35,234 |     825.8 KB |     24 B |      825.9 KB |   2.7% |
| 30 | `java.util.concurrent.ConcurrentHashMap`          |       117 |       7.3 KB |     64 B |      616.4 KB |   2.0% |
| 31 | `java.util.concurrent.ConcurrentHashMap$Node[]`   |        93 |      61.9 KB |   8.0 KB |      609.7 KB |   2.0% |
| 32 | `java.util.LinkedHashMap`                         |     5,826 |     364.1 KB |     64 B |      525.8 KB |   1.7% |
| 33 | `java.util.concurrent.ConcurrentHashMap$Node`     |     7,007 |     219.0 KB |     32 B |      472.3 KB |   1.5% |
| 34 | `java.util.jar.Attributes`                        |     5,786 |      90.4 KB |     16 B |      471.2 KB |   1.5% |
| 35 | `cafesat.asts.core.Trees$PredicateApplication`    |    19,761 |     463.1 KB |     24 B |      463.3 KB |   1.5% |
| 36 | `java.time.zone.ZoneRulesProvider`                |         0 |          0 B |      0 B |      198.4 KB |   0.6% |
| 37 | `java.net.URLClassLoader`                         |         2 |        176 B |     88 B |      184.3 KB |   0.6% |
| 38 | `java.lang.invoke.MethodType`                     |       894 |      34.9 KB |     40 B |      183.4 KB |   0.6% |
| 39 | `java.lang.Integer`                               |     9,789 |     153.0 KB |     16 B |      153.4 KB |   0.5% |
| 40 | `java.util.LinkedHashMap$Entry`                   |     1,200 |      46.9 KB |     40 B |      151.7 KB |   0.5% |
| 41 | `org.renaissance.core.ModuleLoader`               |         2 |         48 B |     24 B |      149.8 KB |   0.5% |
| 42 | `sun.util.calendar.ZoneInfoFile`                  |         0 |          0 B |      0 B |      145.4 KB |   0.5% |
| 43 | `java.util.LinkedHashSet`                         |        38 |        608 B |     16 B |      144.6 KB |   0.5% |
| 44 | `cafesat.api.Formulas$Formula`                    |     8,908 |     139.2 KB |     16 B |      139.2 KB |   0.5% |
| 45 | `java.time.zone.TzdbZoneRulesProvider`            |         1 |         24 B |     24 B |      118.2 KB |   0.4% |
| 46 | `byte[][]`                                        |         1 |       1.4 KB |   1.4 KB |       94.1 KB |   0.3% |
| 47 | `java.util.ImmutableCollections$SetN`             |       149 |       3.5 KB |     24 B |       91.5 KB |   0.3% |
| 48 | `sun.security.util.KnownOIDs`                     |       264 |      10.3 KB |     40 B |       88.5 KB |   0.3% |
| 49 | `java.util.ArrayList`                             |       102 |       2.4 KB |     24 B |       83.6 KB |   0.3% |
| 50 | `org.renaissance.core.BenchmarkSuite`             |         1 |         32 B |     32 B |       77.0 KB |   0.3% |
_… 2,917 more classes, 639.6 KB shallow / 2.9 MB retained (full list in JSON)._

### Class Loaders

_Classes grouped by the loader that defined them. Growing loaders (e.g. web-app or plugin loaders redeployed multiple times) are a common source of metaspace and heap leaks. The **Loader** column shows the loader's class (e.g. `java/net/URLClassLoader`), not an instance name — the hprof format does not record loader names. Multiple rows with the same loader class are distinct loader instances; many such instances each holding significant heap can signal a class-loader leak. The **Address** column distinguishes them._

| Loader                                               | Address    | Classes | Instances | Shallow Heap | Retained Heap |
| ---------------------------------------------------- | ---------- | ------: | --------: | -----------: | ------------: |
| java/net/URLClassLoader                              | 0xc0412288 |     606 |   596,452 |      19.1 MB |       62.3 MB |
| <boot>                                               | <boot>     |   1,703 |   355,819 |      10.7 MB |       61.7 MB |
| java/net/URLClassLoader                              | 0xce800048 |     575 |       330 |      18.0 KB |        2.6 MB |
| jdk/internal/loader/ClassLoaders$AppClassLoader      | 0xffeecf48 |      82 |        64 |       1.3 KB |      235.6 KB |
| jdk/internal/loader/ClassLoaders$PlatformClassLoader | 0xffeec828 |       1 |         1 |         16 B |        7.4 KB |

### Duplicate Classes

_Class names loaded by more than one class loader. The same class loaded N times means N separate copies of its static state and N times the metaspace cost — a typical symptom of class-loader leaks (e.g. each web-app reload or plugin load creates a new loader that never gets GC'd). Check the per-loader breakdown: if one loader holds almost all the instances the others are likely leaked copies._

| Class                                          | #Loaders | Instances | Retained Heap |
| ---------------------------------------------- | -------: | --------: | ------------: |
| `scala.collection.immutable.$colon$colon`      |        2 |   146,181 |        8.6 MB |
| `scala.runtime.ObjectRef`                      |        2 |         1 |      962.2 KB |
| `scala.collection.immutable.HashMap`           |        2 |         2 |       51.7 KB |
| `scala.math.BigInt$`                           |        2 |         2 |       16.3 KB |
| `scala.math.BigInt[]`                          |        2 |         2 |       16.0 KB |
| `scala.collection.mutable.ListBuffer`          |        2 |         1 |        7.4 KB |
| `scala.Option[]`                               |        2 |        18 |        3.5 KB |
| `scala.Some`                                   |        2 |       176 |        3.1 KB |
| `scala.collection.immutable.LazyList$`         |        2 |         2 |        2.0 KB |
| `scala.collection.StrictOptimizedLinearSeqOps` |        2 |         0 |        2.0 KB |
| `scala.collection.mutable.AbstractBuffer`      |        2 |         0 |        1.9 KB |
| `scala.collection.ArrayOps$`                   |        2 |         2 |        1.6 KB |
| `scala.collection.mutable.HashMap`             |        2 |         4 |        1.5 KB |
| `scala.collection.immutable.LazyList`          |        2 |         2 |        1.5 KB |
| `scala.collection.LinearSeqOps`                |        2 |         0 |        1.4 KB |
| `scala.collection.StrictOptimizedIterableOps`  |        2 |         0 |        1.1 KB |
| `scala.collection.immutable.Range`             |        2 |         0 |         952 B |
| `scala.collection.IterableOnceOps`             |        2 |         0 |         888 B |
| `scala.collection.mutable.HashMap$Node[]`      |        2 |         4 |         768 B |
| `scala.collection.immutable.Map`               |        2 |         0 |         736 B |

**`scala.collection.immutable.$colon$colon`** — per loader:

| Loader                                | Instances | Shallow | Retained Heap |
| ------------------------------------- | --------: | ------: | ------------: |
| `java/net/URLClassLoader` @0xc0412288 |   146,151 |  3.3 MB |        8.6 MB |
| `java/net/URLClassLoader` @0xce800048 |        30 |   720 B |        5.8 KB |

**`scala.runtime.ObjectRef`** — per loader:

| Loader                                | Instances | Shallow | Retained Heap |
| ------------------------------------- | --------: | ------: | ------------: |
| `java/net/URLClassLoader` @0xc0412288 |         1 |    16 B |      962.2 KB |
| `java/net/URLClassLoader` @0xce800048 |         0 |     0 B |           8 B |

**`scala.collection.immutable.HashMap`** — per loader:

| Loader                                | Instances | Shallow | Retained Heap |
| ------------------------------------- | --------: | ------: | ------------: |
| `java/net/URLClassLoader` @0xc0412288 |         2 |    32 B |       51.6 KB |
| `java/net/URLClassLoader` @0xce800048 |         0 |     0 B |          24 B |

**`scala.math.BigInt$`** — per loader:

| Loader                                | Instances | Shallow | Retained Heap |
| ------------------------------------- | --------: | ------: | ------------: |
| `java/net/URLClassLoader` @0xc0412288 |         1 |    16 B |        8.2 KB |
| `java/net/URLClassLoader` @0xce800048 |         1 |    16 B |        8.2 KB |

**`scala.math.BigInt[]`** — per loader:

| Loader                                | Instances | Shallow | Retained Heap |
| ------------------------------------- | --------: | ------: | ------------: |
| `java/net/URLClassLoader` @0xc0412288 |         1 |  8.0 KB |        8.0 KB |
| `java/net/URLClassLoader` @0xce800048 |         1 |  8.0 KB |        8.0 KB |

**`scala.collection.mutable.ListBuffer`** — per loader:

| Loader                                | Instances | Shallow | Retained Heap |
| ------------------------------------- | --------: | ------: | ------------: |
| `java/net/URLClassLoader` @0xce800048 |         1 |    32 B |        6.3 KB |
| `java/net/URLClassLoader` @0xc0412288 |         0 |     0 B |        1.0 KB |

**`scala.Option[]`** — per loader:

| Loader                                | Instances | Shallow | Retained Heap |
| ------------------------------------- | --------: | ------: | ------------: |
| `java/net/URLClassLoader` @0xc0412288 |        18 |  1008 B |        3.5 KB |
| `java/net/URLClassLoader` @0xce800048 |         0 |     0 B |           0 B |

**`scala.Some`** — per loader:

| Loader                                | Instances | Shallow | Retained Heap |
| ------------------------------------- | --------: | ------: | ------------: |
| `java/net/URLClassLoader` @0xc0412288 |       158 |  2.5 KB |        2.6 KB |
| `java/net/URLClassLoader` @0xce800048 |        18 |   288 B |         568 B |

**`scala.collection.immutable.LazyList$`** — per loader:

| Loader                                | Instances | Shallow | Retained Heap |
| ------------------------------------- | --------: | ------: | ------------: |
| `java/net/URLClassLoader` @0xc0412288 |         1 |    16 B |        1.0 KB |
| `java/net/URLClassLoader` @0xce800048 |         1 |    16 B |        1.0 KB |

**`scala.collection.StrictOptimizedLinearSeqOps`** — per loader:

| Loader                                | Instances | Shallow | Retained Heap |
| ------------------------------------- | --------: | ------: | ------------: |
| `java/net/URLClassLoader` @0xc0412288 |         0 |     0 B |        1.9 KB |
| `java/net/URLClassLoader` @0xce800048 |         0 |     0 B |          72 B |

**`scala.collection.mutable.AbstractBuffer`** — per loader:

| Loader                                | Instances | Shallow | Retained Heap |
| ------------------------------------- | --------: | ------: | ------------: |
| `java/net/URLClassLoader` @0xc0412288 |         0 |     0 B |        1.8 KB |
| `java/net/URLClassLoader` @0xce800048 |         0 |     0 B |          80 B |

**`scala.collection.ArrayOps$`** — per loader:

| Loader                                | Instances | Shallow | Retained Heap |
| ------------------------------------- | --------: | ------: | ------------: |
| `java/net/URLClassLoader` @0xc0412288 |         1 |    16 B |         936 B |
| `java/net/URLClassLoader` @0xce800048 |         1 |    16 B |         680 B |

**`scala.collection.mutable.HashMap`** — per loader:

| Loader                                | Instances | Shallow | Retained Heap |
| ------------------------------------- | --------: | ------: | ------------: |
| `java/net/URLClassLoader` @0xc0412288 |         1 |    32 B |         848 B |
| `java/net/URLClassLoader` @0xce800048 |         3 |    96 B |         712 B |

**`scala.collection.immutable.LazyList`** — per loader:

| Loader                                | Instances | Shallow | Retained Heap |
| ------------------------------------- | --------: | ------: | ------------: |
| `java/net/URLClassLoader` @0xc0412288 |         1 |    24 B |         744 B |
| `java/net/URLClassLoader` @0xce800048 |         1 |    24 B |         744 B |

**`scala.collection.LinearSeqOps`** — per loader:

| Loader                                | Instances | Shallow | Retained Heap |
| ------------------------------------- | --------: | ------: | ------------: |
| `java/net/URLClassLoader` @0xc0412288 |         0 |     0 B |        1.3 KB |
| `java/net/URLClassLoader` @0xce800048 |         0 |     0 B |         120 B |

**`scala.collection.StrictOptimizedIterableOps`** — per loader:

| Loader                                | Instances | Shallow | Retained Heap |
| ------------------------------------- | --------: | ------: | ------------: |
| `java/net/URLClassLoader` @0xc0412288 |         0 |     0 B |        1016 B |
| `java/net/URLClassLoader` @0xce800048 |         0 |     0 B |         112 B |

**`scala.collection.immutable.Range`** — per loader:

| Loader                                | Instances | Shallow | Retained Heap |
| ------------------------------------- | --------: | ------: | ------------: |
| `java/net/URLClassLoader` @0xc0412288 |         0 |     0 B |         920 B |
| `java/net/URLClassLoader` @0xce800048 |         0 |     0 B |          32 B |

**`scala.collection.IterableOnceOps`** — per loader:

| Loader                                | Instances | Shallow | Retained Heap |
| ------------------------------------- | --------: | ------: | ------------: |
| `java/net/URLClassLoader` @0xc0412288 |         0 |     0 B |         664 B |
| `java/net/URLClassLoader` @0xce800048 |         0 |     0 B |         224 B |

**`scala.collection.mutable.HashMap$Node[]`** — per loader:

| Loader                                | Instances | Shallow | Retained Heap |
| ------------------------------------- | --------: | ------: | ------------: |
| `java/net/URLClassLoader` @0xc0412288 |         1 |    80 B |         400 B |
| `java/net/URLClassLoader` @0xce800048 |         3 |   240 B |         368 B |

**`scala.collection.immutable.Map`** — per loader:

| Loader                                | Instances | Shallow | Retained Heap |
| ------------------------------------- | --------: | ------: | ------------: |
| `java/net/URLClassLoader` @0xce800048 |         0 |     0 B |         640 B |
| `java/net/URLClassLoader` @0xc0412288 |         0 |     0 B |          96 B |

## Leak Suspects

_Objects and class groups retaining the most heap, ranked by retained size. These are the most likely accumulation points for excessive memory usage. To fix: follow the dominator chain to the nearest object you control and drop or null out the reference that keeps it alive. The path to GC root is shown for each suspect below._

### 1. `java.lang.Thread` — retains 22.9 MB (76.7% of reachable heap)

One `java.lang.Thread` object (shallow 104 B) dominates this retained heap.

Held by a **Thread** GC root.

This object is itself the accumulation point (retained 22.9 MB).

_Directly dominates 28,147 objects (showing top 50 classes by retained heap)._

**Accumulated objects by class:**

| Class                                                                  | Objects |  Shallow | Retained | % of suspect |
| ---------------------------------------------------------------------- | ------: | -------: | -------: | -----------: |
| `scala.collection.immutable.HashSet`                                   |       2 |     32 B |   8.0 MB |        35.1% |
| `cafesat.sat.Solver`                                                   |       1 |    168 B |   4.6 MB |        20.2% |
| `scala.collection.immutable.$colon$colon`                              |       2 |     48 B |   4.2 MB |        18.3% |
| `cafesat.asts.core.Trees$ConnectiveApplication`                        |   8,909 | 208.8 KB |   3.9 MB |        17.0% |
| `scala.runtime.ObjectRef`                                              |       1 |     16 B | 962.1 KB |         4.1% |
| `scala.collection.immutable.BitmapIndexedSetNode`                      |       7 |    280 B | 365.5 KB |         1.6% |
| `cafesat.asts.core.Trees$ConnectiveSymbol`                             |   8,748 | 205.0 KB | 205.0 KB |         0.9% |
| `cafesat.api.Formulas$Formula`                                         |   8,908 | 139.2 KB | 139.2 KB |         0.6% |
| `java.util.HashMap`                                                    |       1 |     48 B | 137.1 KB |         0.6% |
| `org.renaissance.core.BenchmarkSuite`                                  |       1 |     32 B |  75.3 KB |         0.3% |
| `org.renaissance.core.ModuleLoader`                                    |       1 |     24 B |  73.8 KB |         0.3% |
| `scala.collection.immutable.HashMap`                                   |       1 |     16 B |  51.5 KB |         0.2% |
| `cafesat.asts.core.Trees$PredicateSymbol`                              |     729 |  17.1 KB |  51.3 KB |         0.2% |
| `scala.collection.immutable.Vector3`                                   |       3 |    144 B |  36.6 KB |         0.2% |
| `cafesat.api.Formulas$Formula[]`                                       |       2 |  35.1 KB |  35.1 KB |         0.2% |
| `cafesat.asts.core.Trees$PredicateApplication`                         |     729 |  17.1 KB |  17.1 KB |         0.1% |
| `cafesat.api.Formulas$PropVar[][][]`                                   |       1 |     56 B |  16.4 KB |         0.1% |
| `org.renaissance.harness.ConfigParser`                                 |       1 |     16 B |   5.8 KB |        <0.1% |
| `java.lang.ThreadLocal$ThreadLocalMap`                                 |       1 |     24 B |   4.4 KB |        <0.1% |
| `scala.collection.immutable.Vector2`                                   |       2 |     64 B |   3.8 KB |        <0.1% |
| `org.renaissance.scala.sat.ScalaDoku`                                  |       1 |     24 B |   2.4 KB |        <0.1% |
| `java.lang.String`                                                     |      33 |    792 B |   1.8 KB |        <0.1% |
| `scala.Option[][]`                                                     |       1 |     56 B |   1.8 KB |        <0.1% |
| `org.renaissance.core.BenchmarkDescriptor`                             |      25 |    600 B |    600 B |        <0.1% |
| `org.renaissance.harness.ExecutionDriver`                              |       1 |     56 B |    400 B |        <0.1% |
| `org.renaissance.harness.Config`                                       |       1 |     72 B |    360 B |        <0.1% |
| `scala.collection.immutable.SetIterator`                               |       2 |     80 B |    320 B |        <0.1% |
| `java.lang.reflect.Method`                                             |       1 |     88 B |    296 B |        <0.1% |
| `org.renaissance.core.BenchmarkSuite$SuiteBenchmarkContext`            |       1 |     24 B |    280 B |        <0.1% |
| `org.renaissance.harness.EventDispatcher`                              |       1 |     56 B |    248 B |        <0.1% |
| `scala.collection.convert.JavaCollectionWrappers$JListWrapper`         |       1 |     16 B |    192 B |        <0.1% |
| `java.lang.String[]`                                                   |       1 |     32 B |    128 B |        <0.1% |
| `scala.collection.mutable.HashMap`                                     |       1 |     32 B |    112 B |        <0.1% |
| `java.lang.invoke.BoundMethodHandle$Species_LL`                        |       2 |     80 B |     80 B |        <0.1% |
| `scala.Tuple3`                                                         |       2 |     48 B |     80 B |        <0.1% |
| `scala.collection.immutable.$colon$colon`                              |       2 |     48 B |     72 B |        <0.1% |
| `java.lang.invoke.MemberName`                                          |       1 |     40 B |     56 B |        <0.1% |
| `org.renaissance.harness.ExecutionPolicies$FixedOpCount`               |       1 |     24 B |     48 B |        <0.1% |
| `scala.collection.convert.JavaCollectionWrappers$MutableMapWrapper`    |       1 |     32 B |     48 B |        <0.1% |
| `java.lang.invoke.DirectMethodHandle`                                  |       1 |     40 B |     40 B |        <0.1% |
| `java.security.AccessControlContext`                                   |       1 |     40 B |     40 B |        <0.1% |
| `java.lang.Thread$FieldHolder`                                         |       1 |     40 B |     40 B |        <0.1% |
| `org.renaissance.harness.RenaissanceSuite$$$Lambda+0x00007de4a41205b8` |       1 |     40 B |     40 B |        <0.1% |
| `sun.nio.fs.UnixPath`                                                  |       1 |     32 B |     32 B |        <0.1% |
| `jdk.internal.reflect.DirectMethodHandleAccessor`                      |       1 |     32 B |     32 B |        <0.1% |
| `java.lang.Object[]`                                                   |       1 |     24 B |     24 B |        <0.1% |
| `scala.collection.mutable.ArrayBuffer`                                 |       1 |     24 B |     24 B |        <0.1% |
| `cafesat.sat.Solver$$Lambda+0x00007de4a41f7bd0`                        |       1 |     24 B |     24 B |        <0.1% |
| `org.renaissance.BenchmarkResult[]`                                    |       1 |     24 B |     24 B |        <0.1% |
| `org.renaissance.scala.sat.ScalaDoku$DokuResult`                       |       1 |     24 B |     24 B |        <0.1% |

**Dominator chain to GC root:**

1. `java.lang.Thread` (22.9 MB) — GC root: Thread (this object is directly held by a GC root; no intermediate chain)

<details>
<summary>Dominator subtree</summary>

**Dominator subtree:**

- `java.lang.Thread` (shallow 104 B, retained 22.9 MB)
  - `scala.collection.immutable.HashSet` (shallow 16 B, retained 8.0 MB)
    - `scala.collection.immutable.BitmapIndexedSetNode` (shallow 40 B, retained 8.0 MB)
      - `java.lang.Object[]` (shallow 144 B, retained 8.0 MB)
        - `scala.collection.immutable.BitmapIndexedSetNode` (shallow 40 B, retained 817.1 KB)
          - `java.lang.Object[]` (shallow 144 B, retained 817.1 KB)
            _… (1 deeper — full data in JSON)_

</details>

### 2. `java.lang.Class` — retains 3.5 MB (11.7% of reachable heap)

1,669 instances of `java.lang.Class` together retain this heap (combined shallow 24.8 KB).

_Note: `java.lang.Class` objects are normal — every loaded class has one. This suspect reflects class-metadata memory, not a leak in application code. It is worth investigating only if the instance count is unexpectedly high (e.g. due to class-loader leaks)._

#### Merged Paths to GC Roots

- `java.lang.Class` (1,669 objects, retained 3.5 MB)
  - `java.time.zone.ZoneRulesProvider` (1 object, retained 198.4 KB) — GC root: Sticky Class
  - `cafesat.sat.Solver$CNFFormula` (1 object, retained 3.8 KB)
  - `sun.text.resources.cldr.FormatData_en` (1 object, retained 2.7 KB) — GC root: Sticky Class
  - `cafesat.common.FixedIntDoublePriorityQueue` (1 object, retained 1.3 KB)
  - `java.time.Month` (1 object, retained 1008 B) — GC root: Sticky Class
  - `java.time.DayOfWeek` (1 object, retained 640 B) — GC root: Sticky Class
  - `java.time.temporal.TemporalAdjusters` (1 object, retained 448 B) — GC root: Sticky Class
  - `scala.collection.mutable.ArraySeq` (1 object, retained 408 B)
  - `scala.Option` (2 objects, retained 400 B)
  - `jdk.internal.logger.SimpleConsoleLogger$Formatting` (1 object, retained 352 B) — GC root: Sticky Class
  - `sun.util.logging.PlatformLogger$Level` (1 object, retained 320 B) — GC root: Sticky Class
  - `java.util.logging.SimpleFormatter` (1 object, retained 280 B) — GC root: Sticky Class
  - `java.time.zone.ZoneOffsetTransitionRule$TimeDefinition` (1 object, retained 232 B) — GC root: Sticky Class
  - `java.time.ZonedDateTime$1` (1 object, retained 144 B) — GC root: Sticky Class
  - `scala.collection.immutable.Set` (1 object, retained 144 B)
  - `java.lang.System$Logger$Level` (1 object, retained 136 B) — GC root: Sticky Class
  - `java.time.ZonedDateTime` (1 object, retained 136 B) — GC root: Sticky Class
  - `scala.Some` (2 objects, retained 128 B)
  - `java.time.zone.ZoneRulesProvider$1` (1 object, retained 112 B) — GC root: Sticky Class
  - `jdk.internal.logger.SimpleConsoleLogger` (1 object, retained 112 B) — GC root: Sticky Class
  - `sun.util.logging.PlatformLogger` (1 object, retained 112 B) — GC root: Sticky Class
  - `scala.collection.mutable.AbstractSeq` (2 objects, retained 96 B)
  - `java.time.chrono.ChronoZonedDateTime` (1 object, retained 96 B) — GC root: Sticky Class
  - `scala.collection.immutable.LazyList$State$Empty$` (2 objects, retained 80 B)
  - `java.time.zone.ZoneOffsetTransition` (1 object, retained 80 B) — GC root: Sticky Class
  - `scala.Function0` (2 objects, retained 64 B)
  - `java.time.zone.TzdbZoneRulesProvider` (1 object, retained 64 B) — GC root: Sticky Class
  - `java.util.Formatter$DateTime` (1 object, retained 64 B) — GC root: Sticky Class
  - `java.time.zone.Ser` (1 object, retained 40 B) — GC root: Sticky Class
  - `java.util.logging.StreamHandler` (1 object, retained 40 B) — GC root: Sticky Class
  - `jdk.internal.logger.SurrogateLogger` (1 object, retained 40 B) — GC root: Sticky Class
  - `java.lang.reflect.InvocationTargetException` (1 object, retained 32 B) — GC root: Sticky Class
  - `java.util.logging.Formatter` (1 object, retained 32 B) — GC root: Sticky Class
  - `java.io.Externalizable` (1 object, retained 24 B) — GC root: Sticky Class
  - `java.nio.file.FileVisitor` (1 object, retained 24 B) — GC root: Sticky Class
  - `jdk.internal.module.ModulePatcher$PatchedModuleReader` (1 object, retained 24 B) — GC root: Sticky Class
  - `java.io.IOException` (1 object, retained 8 B) — GC root: Sticky Class
  - `scala.collection.mutable.ArraySeq$ofRef` (1 object, retained 8 B)
  - `scala.runtime.LongRef` (1 object, retained 8 B)
  - `java.lang.Module$$Lambda+0x00007de4a4041e98` (1 object, retained 0 B) — GC root: Sticky Class
  - `java.lang.System$Logger$Level[]` (1 object, retained 0 B)
  - `java.lang.WeakPairMap$Pair$Weak` (1 object, retained 0 B) — GC root: Sticky Class
  - `java.lang.WeakPairMap$Pair$Weak$1` (1 object, retained 0 B) — GC root: Sticky Class
  - `java.lang.WeakPairMap$WeakRefPeer` (1 object, retained 0 B) — GC root: Sticky Class
  - `java.lang.invoke.MemberName[]` (1 object, retained 0 B)
  - `java.security.ProtectionDomain[]` (1 object, retained 0 B)
  - `java.time.LocalDateTime[]` (1 object, retained 0 B)
  - `java.time.temporal.TemporalAdjusters$$Lambda+0x00007de4a404ad48` (1 object, retained 0 B) — GC root: Sticky Class
  - `java.time.zone.ZoneOffsetTransitionRule[]` (1 object, retained 0 B)
  - `java.util.Arrays$ArrayItr` (1 object, retained 0 B) — GC root: Sticky Class
  - `java.util.Formatter$FixedString` (1 object, retained 0 B) — GC root: Sticky Class
  - `java.util.logging.SimpleFormatter$$Lambda+0x00007de4a4043800` (1 object, retained 0 B) — GC root: Sticky Class
  - `scala.runtime.java8.JFunction1$mcDI$sp` (1 object, retained 0 B)
  - `scala.runtime.java8.JFunction1$mcVI$sp` (1 object, retained 0 B)
  - `sun.net.www.protocol.jrt.Handler` (1 object, retained 0 B) — GC root: Sticky Class
  - `sun.nio.cs.UTF_8$Encoder` (1 object, retained 0 B) — GC root: Sticky Class
  - `sun.util.calendar.ZoneInfoFile$Checksum` (1 object, retained 0 B) — GC root: Sticky Class
  - `sun.util.logging.PlatformLogger$ConfigurableBridge$LoggerConfiguration` (1 object, retained 0 B) — GC root: Sticky Class
  - `sun.util.logging.PlatformLogger$Level[]` (1 object, retained 0 B)

## Top Consumers

_Biggest objects, classes, and packages by retained heap. Unlike Leak Suspects, these tables are unfiltered — use them when a suspect didn't cross the leak threshold, or to see the full retention picture._

### Biggest Objects (Top-Level Dominators)

_All top-level dominators ranked by retained heap — every object directly held by a GC root. Use it when the suspect you care about didn't cross the leak-suspect threshold._

|  # | Class                                             | Shallow | Retained | % Heap |
| -: | ------------------------------------------------- | ------: | -------: | -----: |
|  1 | `java.lang.Thread`                                |   104 B |  22.9 MB |  76.7% |
|  2 | `scala.runtime.LazyVals$`                         |    32 B |   2.5 MB |   8.4% |
|  3 | `java.util.jar.JarFile`                           |    64 B | 584.6 KB |   1.9% |
|  4 | `java.util.jar.JarFile`                           |    64 B | 584.5 KB |   1.9% |
|  5 | `java.util.zip.ZipFile$Source`                    |    80 B | 295.2 KB |   1.0% |
|  6 | `java.util.zip.ZipFile$Source`                    |    80 B | 295.2 KB |   1.0% |
|  7 | `java.time.zone.ZoneRulesProvider`                |    16 B | 198.4 KB |   0.6% |
|  8 | `sun.util.calendar.ZoneInfoFile`                  |   120 B | 145.4 KB |   0.5% |
|  9 | `java.net.URLClassLoader`                         |    88 B | 111.7 KB |   0.4% |
| 10 | `sun.security.util.KnownOIDs`                     |  1.1 KB |  88.5 KB |   0.3% |
| 11 | `java.lang.Object[]`                              |  8.9 KB |  75.6 KB |   0.2% |
| 12 | `java.net.URLClassLoader`                         |    88 B |  72.4 KB |   0.2% |
| 13 | `java.util.zip.ZipFile$Source`                    |    80 B |  68.2 KB |   0.2% |
| 14 | `java.lang.invoke.MethodType`                     |    48 B |  65.0 KB |   0.2% |
| 15 | `java.util.zip.ZipFile$Source`                    |    80 B |  62.8 KB |   0.2% |
| 16 | `sun.util.locale.provider.LocaleProviderAdapter`  |    24 B |  62.3 KB |   0.2% |
| 17 | `jdk.internal.loader.ClassLoaders$AppClassLoader` |    96 B |  57.3 KB |   0.2% |
| 18 | `sun.security.provider.Sun`                       |   104 B |  53.7 KB |   0.2% |
| 19 | `java.util.zip.ZipFile$Source`                    |    80 B |  52.5 KB |   0.2% |
| 20 | `sun.util.resources.Bundles`                      |    24 B |  49.2 KB |   0.2% |

### Biggest Classes by Retained Heap

_Classes ranked by total retained heap. High retained with low shallow means the class is keeping many other objects alive — investigate it in Dominator Analysis._

|  # | Class                                                  | Instances | Retained Heap |
| -: | ------------------------------------------------------ | --------: | ------------: |
|  1 | `java.lang.Thread`                                     |        25 |       22.9 MB |
|  2 | `java.lang.Class`                                      |     1,678 |        3.5 MB |
|  3 | `java.util.jar.JarFile`                                |        10 |        1.2 MB |
|  4 | `java.util.zip.ZipFile$Source`                         |        10 |      827.5 KB |
|  5 | `java.lang.String`                                     |     9,115 |      606.0 KB |
|  6 | `java.net.URLClassLoader`                              |         2 |      184.1 KB |
|  7 | `java.lang.invoke.MethodType`                          |       888 |      118.5 KB |
|  8 | `java.lang.Object[]`                                   |         4 |      106.6 KB |
|  9 | `java.lang.Module`                                     |        61 |       63.1 KB |
| 10 | `java.lang.invoke.LambdaForm`                          |       141 |       57.5 KB |
| 11 | `jdk.internal.loader.ClassLoaders$AppClassLoader`      |         1 |       57.3 KB |
| 12 | `sun.security.provider.Sun`                            |         1 |       53.7 KB |
| 13 | `java.lang.invoke.MethodTypeForm`                      |       176 |       45.9 KB |
| 14 | `java.lang.module.ModuleDescriptor`                    |        62 |       40.0 KB |
| 15 | `jdk.internal.loader.ClassLoaders$PlatformClassLoader` |         1 |       39.6 KB |
| 16 | `java.util.concurrent.ConcurrentHashMap`               |         1 |       34.0 KB |
| 17 | `java.io.PrintStream`                                  |         1 |       17.3 KB |
| 18 | `java.net.URI`                                         |        61 |       11.8 KB |
| 19 | `java.lang.ModuleLayer`                                |         2 |       11.8 KB |
| 20 | `jdk.internal.module.ModuleReferenceImpl`              |        62 |        8.9 KB |

### Top-Dominator Size Distribution

_Retained heap distributed across all 13,474 top-level dominators. The shape reveals whether a handful of large objects dominate the heap or memory is scattered across many small ones._

- Dominators: 13,474
- Smallest / largest retained: 0 B / 22.9 MB
- Median retained: 64 B
- Total retained (top-level): 29.8 MB

|   Size ≤ | Count | % of Dom. |
| -------: | ----: | --------: |
|      1 B |   451 |      3.3% |
|      8 B |   102 |      0.8% |
|     16 B |   258 |      1.9% |
|     32 B |   774 |      5.7% |
|     64 B | 6,898 |     51.2% |
|    128 B | 3,590 |     26.6% |
|    256 B |   750 |      5.6% |
|    512 B |   382 |      2.8% |
|   1.0 KB |   130 |      1.0% |
|   2.0 KB |    53 |      0.4% |
|   4.0 KB |    31 |      0.2% |
|   8.0 KB |    17 |      0.1% |
|  16.0 KB |     9 |      0.1% |
|  32.0 KB |     7 |      0.1% |
|  64.0 KB |     8 |      0.1% |
| 128.0 KB |     6 |     <0.1% |
| 256.0 KB |     2 |     <0.1% |
| 512.0 KB |     2 |     <0.1% |
|   1.0 MB |     2 |     <0.1% |
|   4.0 MB |     1 |     <0.1% |
|  32.0 MB |     1 |     <0.1% |

### Biggest Packages by Retained Heap

_Retained heap aggregated by package prefix (rows retaining <1% of the total are pruned)._

| Package            | Objects |  Shallow | Retained |
| ------------------ | ------: | -------: | -------: |
| `java`             |  12,512 | 358.8 KB |  26.6 MB |
| `java.lang`        |  11,443 | 333.7 KB |  24.1 MB |
| `java.lang.invoke` |   1,609 |  59.3 KB | 326.7 KB |
| `java.util`        |     579 |  10.2 KB |   2.0 MB |
| `java.util.jar`    |      33 |   1.1 KB |   1.2 MB |
| `java.util.zip`    |      77 |   2.8 KB | 836.5 KB |
| `scala`            |     127 |   1.2 KB |   2.5 MB |
| `scala.runtime`    |      17 |    128 B |   2.5 MB |
| `sun`              |     265 |   6.9 KB | 465.7 KB |

## Dominator Analysis

_Instances ranked by retained heap. An object **dominates** another if every path from a GC root to that object passes through it — making the dominator unreachable reclaims everything it dominates._

### Big Drops

_Objects retaining far more than their largest single child — memory held directly in the object or spread across many small dominated children. Drop = object retained − largest child retained (memory reclaimed if this object became unreachable, net of what the biggest child already accounts for). Threshold 0.3 MB (1% of reachable heap). Multiple rows with the same class are distinct objects._

| Object                                    |      # |    Retained | Largest Child                                     | Child Retained |        Drop |
| ----------------------------------------- | -----: | ----------: | ------------------------------------------------- | -------------: | ----------: |
| `java.lang.Thread`                        | 883792 |     22.9 MB | `scala.collection.immutable.HashSet`              |         8.0 MB |     14.8 MB |
| `java.lang.Object[]`                      | 312335 |      8.0 MB | `scala.collection.immutable.BitmapIndexedSetNode` |       817.1 KB |      7.2 MB |
| `cafesat.sat.Vector[]`                    | 418848 |      4.3 MB | `cafesat.sat.Vector`                              |          136 B |      4.3 MB |
| `java.lang.Object[]`                      |      1 |      2.5 MB | `java.lang.Object`                                |           16 B |      2.5 MB |
| `java.util.HashMap$Node[]`                |     ×2 |    577.2 KB | `java.util.HashMap$Node`                          |         1000 B |    576.2 KB |
| `java.lang.Object[]`                      | 507329 |    578.0 KB | `scala.collection.immutable.BitmapIndexedSetNode` |        20.8 KB |    557.3 KB |
| `java.lang.Object[]`                      |  94766 |    343.1 KB | `scala.collection.immutable.BitmapIndexedSetNode` |        12.8 KB |    330.2 KB |
| `cafesat.sat.Solver`                      | 264493 |      4.6 MB | `cafesat.sat.Vector[]`                            |         4.3 MB |    304.2 KB |
| `java.lang.Object[]`                      | 313366 |    817.1 KB | `scala.collection.immutable.BitmapIndexedSetNode` |       585.4 KB |    231.6 KB |
| `scala.collection.immutable.$colon$colon` | 112989 |      3.2 MB | `scala.collection.immutable.$colon$colon`         |         3.2 MB |     34.9 KB |
| `java.lang.Object[]`                      | 440492 |    585.4 KB | `scala.collection.immutable.BitmapIndexedSetNode` |       578.3 KB |      7.1 KB |
| `java.util.jar.Manifest`                  |     ×2 |    584.2 KB | `java.util.HashMap`                               |       577.2 KB |      6.9 KB |
| `java.lang.Class`                         | 876569 |      2.5 MB | `java.lang.Object[]`                              |         2.5 MB |       920 B |
| `scala.collection.immutable.$colon$colon` |    ×10 |      1.8 MB | `scala.collection.immutable.$colon$colon`         |         1.8 MB |       504 B |
| **Total**                                 |        | **71.0 MB** |                                                   |    **39.6 MB** | **31.5 MB** |

### Immediate Dominators

_One row per dominator class: how many other objects it immediately dominates and the total shallow heap of those dominated objects. A large dominated-shallow figure means instances of that class are collectively gating large portions of the live heap — making them unreachable would allow that memory to be reclaimed._

| Dominator Class                                   | #Dominators |  #Dominated | Dominator Shallow | Dominated Shallow |
| ------------------------------------------------- | ----------: | ----------: | ----------------: | ----------------: |
| `scala.collection.immutable.$colon$colon`         |     128,495 |     221,107 |            2.9 MB |            5.6 MB |
| `java.lang.Object[]`                              |      23,311 |     231,148 |            1.3 MB |            4.7 MB |
| `scala.collection.immutable.Set$Set2`             |      44,628 |      89,256 |            1.0 MB |            3.4 MB |
| `cafesat.sat.Vector`                              |      36,614 |      36,615 |          858.1 KB |            3.4 MB |
| `cafesat.sat.Solver$Clause`                       |      65,565 |      65,565 |            2.0 MB |            1.6 MB |
| `cafesat.asts.core.Trees$ConnectiveApplication`   |      43,982 |      70,468 |            1.0 MB |            1.6 MB |
| `scala.collection.immutable.BitmapIndexedSetNode` |      22,791 |      45,200 |          890.3 KB |            1.4 MB |
| `java.lang.String`                                |      23,882 |      23,882 |          559.7 KB |            1.1 MB |
| `scala.collection.immutable.Set$Set3`             |       8,748 |      26,244 |          205.0 KB |            1.0 MB |
| `cafesat.sat.Vector[]`                            |           1 |      36,616 |          143.0 KB |          858.2 KB |
| `java.util.zip.ZipFile$Source`                    |          10 |          30 |             800 B |          826.7 KB |
| `java.lang.Class`                                 |       1,947 |       4,935 |           32.2 KB |          767.5 KB |
| `java.lang.Thread`                                |          27 |      28,251 |            2.7 KB |          628.9 KB |
| `cafesat.sat.Solver`                              |           1 |           9 |             168 B |          375.6 KB |
| `java.util.jar.Attributes`                        |       5,786 |       5,786 |           90.4 KB |          361.6 KB |
| `java.util.HashMap$Node`                          |       7,677 |      16,139 |          239.9 KB |          354.5 KB |
| `java.util.HashMap$Node[]`                        |         348 |       7,671 |           86.8 KB |          239.9 KB |
| `java.util.concurrent.ConcurrentHashMap$Node[]`   |          91 |       5,779 |           61.7 KB |          211.1 KB |
| `java.util.concurrent.ConcurrentHashMap$Node`     |       5,097 |       6,647 |          159.3 KB |          210.8 KB |
| `byte[][]`                                        |           1 |         346 |            1.4 KB |           92.8 KB |
| `java.util.HashMap`                               |         348 |         400 |           16.3 KB |           87.6 KB |
| `cafesat.common.FixedIntStack`                    |           1 |           1 |              24 B |           71.5 KB |
| `java.util.concurrent.ConcurrentHashMap`          |          93 |         102 |            5.8 KB |           62.1 KB |
| `java.util.LinkedHashMap`                         |          47 |       1,251 |            2.9 KB |           56.7 KB |
| `java.lang.Object[][]`                            |          11 |         269 |            1.2 KB |           37.8 KB |
| `java.lang.invoke.MethodTypeForm`                 |         217 |         434 |            6.8 KB |           32.2 KB |
| `java.io.BufferedWriter`                          |           2 |           4 |              80 B |           32.1 KB |
| `java.lang.invoke.MethodType`                     |         758 |       1,123 |           29.6 KB |           31.5 KB |
| `java.util.LinkedHashMap$Entry`                   |       1,140 |       1,167 |           44.5 KB |           27.4 KB |
| `jdk.internal.math.FDBigInteger`                  |         341 |         341 |           10.7 KB |           24.2 KB |
| **Total**                                         | **421,960** | **926,786** |       **11.6 MB** |       **29.1 MB** |

## Threads

_Per-thread call stacks and retained heap. A thread keeps everything on its stack alive — blocked or long-running threads can hold significant memory through local variables._

### Thread Overview

_Per-thread retained heap and properties. A thread keeps everything on its stack alive — blocked or long-running threads can hold significant memory through local variables._

| Name                           | Shallow | Retained | Max. Locals' Retained | Context Class Loader                   | Daemon | Priority | State                                                  |
| ------------------------------ | ------: | -------: | --------------------: | -------------------------------------- | ------ | -------: | ------------------------------------------------------ |
| [main](#thread-1)              |   104 B |  22.9 MB |                4.6 MB | `java/net/URLClassLoader @ 0xc0412288` | no     |        5 | [alive, runnable]                                      |
| [Reference Handler](#thread-2) |   104 B |    200 B |                   0 B | `—`                                    | yes    |       10 | [alive, runnable]                                      |
| [Finalizer](#thread-3)         |   112 B |    208 B |                  40 B | `—`                                    | yes    |        8 | [alive, waiting, waiting indefinitely, in Object.wait] |
| [Common-Cleaner](#thread-6)    |   112 B |    168 B |                 128 B | `—`                                    | yes    |        8 | [alive, waiting, waiting with timeout, parked]         |

<a id="thread-1"></a>

### Thread 1 "main" (java/lang/Thread)

_Local roots: 124._

_Showing top 20 by retained heap (sizes overlap and do not sum to thread total)._

**Local root objects:**

| Object                                          | Count | Shallow | Retained |
| ----------------------------------------------- | ----: | ------: | -------: |
| `cafesat.sat.Solver`                            |    ×2 |   168 B |   4.6 MB |
| `scala.collection.immutable.$colon$colon`       |     1 |    24 B |   3.2 MB |
| `scala.collection.immutable.$colon$colon`       |     1 |    24 B | 962.2 KB |
| `scala.runtime.ObjectRef`                       |    ×2 |    16 B | 962.1 KB |
| `cafesat.asts.core.Trees$ConnectiveApplication` |     1 |    24 B | 208.8 KB |
| `scala.collection.immutable.SetIterator`        |    ×2 |    40 B |    160 B |
| `cafesat.sat.Solver$$Lambda+0x00007de4a41f7bd0` |     1 |    24 B |     24 B |
| `scala.collection.immutable.HashSet`            |    ×3 |    16 B |     16 B |
| `cafesat.sat.Solver$$Lambda+0x00007de4a41f5a08` |    ×3 |    16 B |     16 B |
| `scala.collection.immutable.Nil$`               |     1 |    16 B |     16 B |
| `cafesat.api.Solver$`                           |     1 |    16 B |     16 B |
| `cafesat.sat.Literal[]`                         |     1 |    16 B |     16 B |
| `cafesat.api.Formulas$Formula`                  |     1 |    16 B |     16 B |

_Frame percentages are of this thread's 22.9 MB retained heap._

- `scala.collection.IterableOnceOps.count (IterableOnce.scala:618)`
  - `scala.collection.immutable.SetIterator` retains 160 B (<0.1% of thread retained)
- `scala.collection.IterableOnceOps.exists (IterableOnce.scala:604)`
  - `scala.collection.immutable.SetIterator` retains 160 B (<0.1% of thread retained)
  - `cafesat.sat.Solver$$Lambda+0x00007de4a41f5a08` retains 16 B (<0.1% of thread retained)
- `scala.collection.IterableOnceOps.exists$ (IterableOnce.scala:601)`
  - `cafesat.sat.Solver$$Lambda+0x00007de4a41f5a08` retains 16 B (<0.1% of thread retained)
  - `scala.collection.immutable.HashSet` retains 16 B (<0.1% of thread retained)
- `scala.collection.AbstractIterable.exists (Iterable.scala:933)`
  - `cafesat.sat.Solver$$Lambda+0x00007de4a41f5a08` retains 16 B (<0.1% of thread retained)
  - `scala.collection.immutable.HashSet` retains 16 B (<0.1% of thread retained)
- `cafesat.sat.Solver.$anonfun$initClauses$1 (Solver.scala:124)`
  - `scala.runtime.ObjectRef` retains 962.1 KB (3.2% of thread retained)
  - `scala.collection.immutable.HashSet` retains 16 B (<0.1% of thread retained)
- `scala.collection.immutable.List.foreach (List.scala:333)`
  - `scala.collection.immutable.$colon$colon` retains 3.2 MB (10.9% of thread retained)
  - `cafesat.sat.Solver$$Lambda+0x00007de4a41f7bd0` retains 24 B (<0.1% of thread retained)
- `cafesat.sat.Solver.initClauses (Solver.scala:115)`
  - `cafesat.sat.Solver` retains 4.6 MB (15.5% of thread retained)
  - `scala.collection.immutable.$colon$colon` retains 962.2 KB (3.2% of thread retained)
  - `scala.runtime.ObjectRef` retains 962.1 KB (3.2% of thread retained)
- `cafesat.sat.Solver.solve (Solver.scala:147)`
  - `cafesat.sat.Solver` retains 4.6 MB (15.5% of thread retained)
  - `cafesat.sat.Literal[]` retains 16 B (<0.1% of thread retained)
  - `scala.collection.immutable.Nil$` retains 16 B (<0.1% of thread retained)
- `cafesat.api.Solver$.solveForSatisfiability (Solver.scala:84)`
  - `cafesat.asts.core.Trees$ConnectiveApplication` retains 208.8 KB (0.7% of thread retained)
  - `cafesat.api.Formulas$Formula` retains 16 B (<0.1% of thread retained)
  - `cafesat.api.Solver$` retains 16 B (<0.1% of thread retained)

<a id="thread-2"></a>

### Thread 2 "Reference Handler" (java/lang/ref/Reference$ReferenceHandler)

_Local roots: 0._

- `java.lang.ref.Reference.waitForReferencePendingList (Native Method)`
- `java.lang.ref.Reference.processPendingReferences (Reference.java:246)`
- `java.lang.ref.Reference$ReferenceHandler.run (Reference.java:208)`

<a id="thread-3"></a>

### Thread 3 "Finalizer" (java/lang/ref/Finalizer$FinalizerThread)

_Local roots: 7._

**Local root objects:**

| Object                                    | Count | Shallow | Retained |
| ----------------------------------------- | ----: | ------: | -------: |
| `java.lang.ref.NativeReferenceQueue`      |    ×3 |    40 B |     40 B |
| `java.lang.ref.NativeReferenceQueue$Lock` |    ×3 |    16 B |     16 B |
| `java.lang.System$2`                      |     1 |    16 B |     16 B |

_Frame percentages are of this thread's 208 B retained heap._

- `java.lang.Object.wait (Object.java:366)`
  - `java.lang.ref.NativeReferenceQueue$Lock` retains 16 B (<0.1% of thread retained)
- `java.lang.Object.wait (Object.java:339)`
  - `java.lang.ref.NativeReferenceQueue$Lock` retains 16 B (<0.1% of thread retained)
- `java.lang.ref.NativeReferenceQueue.await (NativeReferenceQueue.java:48)`
  - `java.lang.ref.NativeReferenceQueue` retains 40 B (<0.1% of thread retained)
- `java.lang.ref.ReferenceQueue.remove0 (ReferenceQueue.java:158)`
  - `java.lang.ref.NativeReferenceQueue` retains 40 B (<0.1% of thread retained)
- `java.lang.ref.NativeReferenceQueue.remove (NativeReferenceQueue.java:89)`
  - `java.lang.ref.NativeReferenceQueue` retains 40 B (<0.1% of thread retained)
  - `java.lang.ref.NativeReferenceQueue$Lock` retains 16 B (<0.1% of thread retained)
- `java.lang.ref.Finalizer$FinalizerThread.run (Finalizer.java:173)`
  - `java.lang.System$2` retains 16 B (<0.1% of thread retained)

<a id="thread-6"></a>

### Thread 6 "Common-Cleaner" (jdk/internal/misc/InnocuousThread)

_Local roots: 12._

**Local root objects:**

| Object                                                                  | Count | Shallow | Retained |
| ----------------------------------------------------------------------- | ----: | ------: | -------: |
| `java.lang.Class`                                                       |    ×2 |    32 B |    128 B |
| `java.util.concurrent.TimeUnit`                                         |     1 |    80 B |     80 B |
| `java.lang.ref.ReferenceQueue`                                          |    ×3 |    32 B |     48 B |
| `java.util.concurrent.locks.AbstractQueuedSynchronizer$ConditionNode`   |     1 |    32 B |     32 B |
| `java.util.concurrent.locks.AbstractQueuedSynchronizer$ConditionObject` |    ×2 |    24 B |     24 B |
| `jdk.internal.ref.CleanerImpl`                                          |    ×3 |    24 B |     24 B |

_Frame percentages are of this thread's 168 B retained heap._

- `java.util.concurrent.locks.LockSupport.parkNanos (LockSupport.java:269)`
  - `java.util.concurrent.locks.AbstractQueuedSynchronizer$ConditionObject` retains 24 B (<0.1% of thread retained)
- `java.util.concurrent.locks.AbstractQueuedSynchronizer$ConditionObject.await (AbstractQueuedSynchronizer.java:1886)`
  - `java.util.concurrent.TimeUnit` retains 80 B (<0.1% of thread retained)
  - `java.util.concurrent.locks.AbstractQueuedSynchronizer$ConditionNode` retains 32 B (<0.1% of thread retained)
  - `java.util.concurrent.locks.AbstractQueuedSynchronizer$ConditionObject` retains 24 B (<0.1% of thread retained)
- `java.lang.ref.ReferenceQueue.await (ReferenceQueue.java:71)`
  - `java.lang.ref.ReferenceQueue` retains 48 B (<0.1% of thread retained)
- `java.lang.ref.ReferenceQueue.remove0 (ReferenceQueue.java:143)`
  - `java.lang.ref.ReferenceQueue` retains 48 B (<0.1% of thread retained)
- `java.lang.ref.ReferenceQueue.remove (ReferenceQueue.java:218)`
  - `java.lang.ref.ReferenceQueue` retains 48 B (<0.1% of thread retained)
- `jdk.internal.ref.CleanerImpl.run (CleanerImpl.java:140)`
  - `jdk.internal.ref.CleanerImpl` retains 24 B (<0.1% of thread retained)
- `java.lang.Thread.runWith (Thread.java:1596)`
  - `java.lang.Class` retains 128 B (<0.1% of thread retained)
  - `jdk.internal.ref.CleanerImpl` retains 24 B (<0.1% of thread retained)
- `java.lang.Thread.run (Thread.java:1583)`
  - `java.lang.Class` retains 128 B (<0.1% of thread retained)
  - `jdk.internal.ref.CleanerImpl` retains 24 B (<0.1% of thread retained)

## Top Components

_Retained heap grouped by class loader (component). `% Heap` is the share of total reachable heap. Totals can exceed heap size because boot-loader classes are counted in every component that retains them._

| Component                                              | Retained | % Heap | Top classes                                                                                                                                                                                                                                   |
| ------------------------------------------------------ | -------: | -----: | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `java/net/URLClassLoader`                              |  62.3 MB |  49.1% | `scala.collection.immutable.$colon$colon` (8.6 MB), `scala.collection.immutable.BitmapIndexedSetNode` (8.4 MB), `scala.collection.immutable.HashSet` (8.0 MB), `cafesat.sat.Literal` (4.8 MB), `cafesat.sat.Solver` (4.6 MB)                  |
| `<boot>`                                               |  61.7 MB |  48.7% | `java.lang.Thread` (22.9 MB), `java.lang.Object[]` (11.4 MB), `java.lang.Class` (3.5 MB), `int[]` (2.6 MB), `java.lang.Object` (2.0 MB)                                                                                                       |
| `java/net/URLClassLoader`                              |   2.6 MB |   2.1% | `scala.runtime.LazyVals$` (2.5 MB), `org.renaissance.harness.ConfigParser$$anon$1` (13.0 KB), `scala.math.BigInt$` (8.2 KB), `scopt.ORunner$` (8.1 KB), `scala.math.BigInt[]` (8.0 KB)                                                        |
| `jdk/internal/loader/ClassLoaders$AppClassLoader`      | 235.6 KB |   0.2% | `org.renaissance.core.ModuleLoader` (149.8 KB), `org.renaissance.core.BenchmarkSuite` (77.0 KB), `org.renaissance.core.BenchmarkDescriptor` (2.3 KB), `org.renaissance.core.Launcher` (1.6 KB), `org.renaissance.core.ResourceUtils` (1.2 KB) |
| `jdk/internal/loader/ClassLoaders$PlatformClassLoader` |   7.4 KB |  <0.1% | `sun.util.resources.cldr.provider.CLDRLocaleDataMetaInfo` (7.4 KB)                                                                                                                                                                            |

## Arrays by Size

_Array-length distribution bucketed by power-of-two element length. Helps spot unexpectedly large arrays or many tiny zero-length allocations. `Max length` is the inclusive upper bound of each bucket._

### Object arrays

| Max length |    Objects |    Shallow |
| ---------: | ---------: | ---------: |
|        ≤ 1 |      1,235 |    28.9 KB |
|        ≤ 2 |     12,712 |   297.9 KB |
|        ≤ 4 |      7,554 |   236.1 KB |
|        ≤ 8 |      2,755 |   115.7 KB |
|       ≤ 16 |      1,755 |   112.2 KB |
|       ≤ 32 |     38,515 |     3.6 MB |
|       ≤ 64 |        119 |    23.3 KB |
|      ≤ 128 |         80 |    27.8 KB |
|      ≤ 256 |         30 |    25.9 KB |
|      ≤ 512 |         10 |    15.4 KB |
|    ≤ 1,024 |         18 |    60.4 KB |
|    ≤ 2,048 |          6 |    40.8 KB |
|    ≤ 4,096 |          6 |    65.3 KB |
|    ≤ 8,192 |          1 |    31.0 KB |
|   ≤ 16,384 |          1 |    34.8 KB |
|   ≤ 32,768 |          1 |    71.5 KB |
|   ≤ 65,536 |          1 |   143.0 KB |
|  ≤ 131,072 |          1 |   512.0 KB |
|  **Total** | **64,800** | **5.4 MB** |

### Primitive arrays

| Max length |     Objects |    Shallow |
| ---------: | ----------: | ---------: |
|        ≤ 1 |         814 |    19.1 KB |
|        ≤ 2 |      67,597 |     1.5 MB |
|        ≤ 4 |      18,501 |   569.0 KB |
|        ≤ 8 |       5,167 |   162.6 KB |
|       ≤ 16 |       5,752 |   224.5 KB |
|       ≤ 32 |       6,779 |   304.0 KB |
|       ≤ 64 |       8,570 |   546.1 KB |
|      ≤ 128 |       1,138 |   112.3 KB |
|      ≤ 256 |         195 |    46.5 KB |
|      ≤ 512 |         270 |   104.2 KB |
|    ≤ 1,024 |          77 |    64.3 KB |
|    ≤ 2,048 |          16 |    62.2 KB |
|    ≤ 4,096 |           3 |     9.6 KB |
|    ≤ 8,192 |           8 |    71.4 KB |
|   ≤ 16,384 |           5 |   130.3 KB |
|   ≤ 32,768 |           5 |   253.0 KB |
|   ≤ 65,536 |           3 |   157.9 KB |
|  ≤ 131,072 |           1 |   512.0 KB |
|  ≤ 262,144 |           2 |   510.2 KB |
|  **Total** | **114,903** | **5.3 MB** |

Zero-length arrays: 2,401

## Collections

_Collection fill ratios, map load factors, and constant-value primitive array groups. Low fill ratios waste backing-array memory; high load factors increase hash-bucket collisions and degrade lookup performance._

### Collections by Kind

| Kind      |     Count | Total Elements | Max Elements | Total Shallow |
| --------- | --------: | -------------: | -----------: | ------------: |
| list      |       105 |          1,261 |          485 |        2.5 KB |
| map       |     6,202 |         11,394 |        2,889 |      381.8 KB |
| **Total** | **6,307** |     **12,655** |              |  **384.3 KB** |

### Collection Fill Ratio

_609 tracked of 6,307 collections._

|      Fill % | Collections |     Shallow |       Wasted |
| ----------: | ----------: | ----------: | -----------: |
|       0–10% |         252 |     11.6 KB |      28.8 KB |
|      10–20% |         136 |      6.3 KB |      15.8 KB |
|      20–30% |          50 |      2.3 KB |       5.5 KB |
|      30–40% |          44 |      2.4 KB |      50.7 KB |
|      40–50% |          45 |      2.4 KB |      41.3 KB |
|      50–60% |          16 |       800 B |       7.3 KB |
|      60–70% |          14 |       776 B |       5.9 KB |
|      70–80% |          12 |       496 B |      19.3 KB |
|      80–90% |           2 |        48 B |       1.2 KB |
|     90–100% |           0 |         0 B |          0 B |
| 100% (full) |          38 |       912 B |          0 B |
|   **Total** |     **609** | **28.0 KB** | **175.8 KB** |

### Collections by Size

_6,307 tracked; 5,806 empty._

|    Size ≤ | Collections | Total Shallow |
| --------: | ----------: | ------------: |
|       ≤ 1 |         204 |        9.0 KB |
|       ≤ 2 |          94 |        3.6 KB |
|       ≤ 4 |         100 |        4.1 KB |
|       ≤ 8 |          37 |        1.8 KB |
|      ≤ 16 |          31 |        1.5 KB |
|      ≤ 32 |           9 |         408 B |
|      ≤ 64 |           8 |         392 B |
|     ≤ 128 |           4 |         224 B |
|     ≤ 256 |           5 |         304 B |
|     ≤ 512 |           4 |         144 B |
|   ≤ 1,024 |           3 |         144 B |
|   ≤ 4,096 |           2 |          96 B |
| **Total** |     **501** |   **21.8 KB** |

### Array Fill Ratio

_64,800 tracked object arrays._

|      Fill % |     Arrays |    Shallow |     Wasted |
| ----------: | ---------: | ---------: | ---------: |
|       0–10% |     37,926 |     3.5 MB |     5.9 MB |
|      10–20% |        193 |    27.2 KB |    40.3 KB |
|      20–30% |         94 |     8.9 KB |    10.9 KB |
|      30–40% |        126 |    52.4 KB |    67.0 KB |
|      40–50% |        245 |   104.4 KB |   105.4 KB |
|      50–60% |         46 |    10.2 KB |     8.8 KB |
|      60–70% |         40 |     5.1 KB |     3.2 KB |
|      70–80% |         30 |     3.4 KB |     1.5 KB |
|      80–90% |         15 |     6.3 KB |     1.7 KB |
|     90–100% |         16 |    36.8 KB |     2.1 KB |
| 100% (full) |     26,069 |     1.6 MB |        0 B |
|   **Total** | **64,800** | **5.4 MB** | **6.1 MB** |

### Map Load Factor

_493 tracked of 6,202 maps (occupied slots ÷ capacity; high values ≥ 90% increase collision chains)._

|      Load % |    Maps |     Shallow |
| ----------: | ------: | ----------: |
|       0–10% |     221 |     11.0 KB |
|      10–20% |     121 |      6.0 KB |
|      20–30% |      40 |      2.2 KB |
|      30–40% |      48 |      2.7 KB |
|      40–50% |      50 |      2.7 KB |
|      50–60% |       8 |       448 B |
|      60–70% |       2 |       128 B |
|      70–80% |       3 |       160 B |
|      80–90% |       0 |         0 B |
|     90–100% |       0 |         0 B |
| 100% (full) |       0 |         0 B |
|   **Total** | **493** | **25.3 KB** |

### Constant Primitive Arrays

_Primitive arrays whose every element is identical — possible candidates for deduplication or replacement with a shared constant. Short arrays (length < 8 with few instances) are hidden as noise._

_(34 trivial groups hidden.)_

| Array class |  Length |       Value | Objects |  Shallow |
| ----------- | ------: | ----------: | ------: | -------: |
| `int[]`     | 131,064 |           0 |       1 | 512.0 KB |
| `int[]`     |  18,307 |           0 |       1 |  71.5 KB |
| `boolean[]` |  18,307 |           0 |       1 |  17.9 KB |
| `char[]`    |   8,192 |           0 |       1 |  16.0 KB |
| `int[]`     |   1,024 | -1059448624 |       1 |   4.0 KB |
| `int[]`     |     802 |           0 |       1 |   3.1 KB |
| `int[]`     |     616 |           0 |       1 |   2.4 KB |
| `long[]`    |      32 |           0 |       4 |   1.1 KB |
| `byte[]`    |     512 |           0 |       2 |   1.0 KB |
| `byte[]`    |       2 |          49 |      31 |    744 B |
| `int[]`     |      32 |           0 |       4 |    576 B |
| `byte[]`    |     256 |           0 |       2 |    544 B |
| `short[]`   |      32 |           0 |       4 |    320 B |
| `int[]`     |       2 |           0 |      11 |    264 B |
| `byte[]`    |       8 |           0 |      10 |    240 B |
| `int[]`     |      10 |           0 |       4 |    224 B |
| `char[]`    |      26 |           0 |       2 |    144 B |
| `byte[]`    |     128 |           0 |       1 |    144 B |
| `byte[]`    |      63 |          48 |       1 |     80 B |
| `int[]`     |      16 |  -807617080 |       1 |     80 B |
| `int[]`     |      16 |  -807616616 |       1 |     80 B |
| `int[]`     |      16 |  -807616152 |       1 |     80 B |
| `int[]`     |      16 |  -807262528 |       1 |     80 B |
| `int[]`     |      12 |           0 |       1 |     64 B |
| `byte[]`    |      10 |          32 |       1 |     32 B |
| `byte[]`    |      13 |          48 |       1 |     32 B |
| `byte[]`    |      16 |           0 |       1 |     32 B |
| `byte[]`    |       8 |          32 |       1 |     24 B |
| `byte[]`    |       8 |          48 |       1 |     24 B |

### Top Arrays (primitive)

_The largest primitive arrays by shallow size, individually and aggregated by array class._

| Array class |  Length |    Shallow | Owner (Class#field)                  |
| ----------- | ------: | ---------: | ------------------------------------ |
| `int[]`     | 131,064 |   512.0 KB | —                                    |
| `byte[]`    | 261,187 |   255.1 KB | `java.util.zip.ZipFile$Source#cen`   |
| `byte[]`    | 261,187 |   255.1 KB | —                                    |
| `int[]`     |  18,307 |    71.5 KB | `cafesat.common.FixedIntStack#stack` |
| `int[]`     |  18,307 |    71.5 KB | `cafesat.sat.Solver#levels`          |
| `int[]`     |  18,307 |    71.5 KB | `cafesat.sat.Solver#model`           |
| `byte[]`    |  60,231 |    58.8 KB | —                                    |
| `byte[]`    |  55,240 |    54.0 KB | `java.util.zip.ZipFile$Source#cen`   |
| `byte[]`    |  46,187 |    45.1 KB | —                                    |
| `int[]`     |   8,908 |    34.8 KB | `cafesat.sat.Solver$Clause#lits`     |
| **Total**   |         | **1.4 MB** |                                      |

#### Top Array Classes (primitive)

| Array class |   Instances |    Shallow |
| ----------- | ----------: | ---------: |
| `int[]`     |      90,907 |     3.2 MB |
| `byte[]`    |      25,805 |     2.1 MB |
| `char[]`    |         224 |    55.8 KB |
| `boolean[]` |          11 |    19.8 KB |
| `long[]`    |          17 |    14.2 KB |
| `double[]`  |           8 |      688 B |
| `short[]`   |           7 |      368 B |
| `float[]`   |           4 |      112 B |
| **Total**   | **116,983** | **5.3 MB** |

### Top Arrays (object)

_The largest object arrays by shallow size, individually and aggregated by array class._

| Array class                      |  Length |     Used/Length |      Shallow | Owner (Class#field)          |
| -------------------------------- | ------: | --------------: | -----------: | ---------------------------- |
| `java.lang.Object[]`             | 131,072 | 131,072/131,072 |     512.0 KB | —                            |
| `cafesat.sat.Vector[]`           |  36,614 |   36,614/36,614 |     143.0 KB | `cafesat.sat.Solver#watched` |
| `cafesat.sat.Solver$Clause[]`    |  18,307 |        0/18,307 |      71.5 KB | `cafesat.sat.Solver#reasons` |
| `cafesat.api.Formulas$Formula[]` |   8,907 |     8,907/8,907 |      34.8 KB | —                            |
| `java.lang.Object[]`             |   7,937 |     7,706/7,937 |      31.0 KB | —                            |
| `java.util.HashMap$Node[]`       |   4,096 |     2,048/4,096 |      16.0 KB | `java.util.HashMap#table`    |
| `java.util.HashMap$Node[]`       |   4,096 |     2,048/4,096 |      16.0 KB | `java.util.HashMap#table`    |
| `java.lang.Object[]`             |   2,285 |       413/2,285 |       8.9 KB | —                            |
| `java.lang.Object[]`             |   2,126 |     1,063/2,126 |       8.3 KB | —                            |
| `scala.math.BigInt[]`            |   2,049 |         0/2,049 |       8.0 KB | —                            |
| **Total**                        |         |                 | **849.7 KB** |                              |

#### Top Array Classes (object)

| Array class                                     |  Instances |    Shallow |
| ----------------------------------------------- | ---------: | ---------: |
| `cafesat.sat.Solver$Clause[]`                   |     36,615 |     3.4 MB |
| `java.lang.Object[]`                            |     25,556 |     1.5 MB |
| `cafesat.sat.Vector[]`                          |          1 |   143.0 KB |
| `java.util.HashMap$Node[]`                      |        396 |    96.6 KB |
| `java.util.concurrent.ConcurrentHashMap$Node[]` |         93 |    61.9 KB |
| `cafesat.api.Formulas$Formula[]`                |          2 |    35.1 KB |
| `java.lang.ref.SoftReference[]`                 |        434 |    32.2 KB |
| `java.lang.Class[]`                             |        729 |    22.3 KB |
| `java.lang.String[]`                            |        207 |    13.2 KB |
| `java.lang.invoke.MethodHandle[]`               |         61 |    11.4 KB |
| **Total**                                       | **64,094** | **5.3 MB** |

## Collection Waste Budget

_Memory tied up in avoidable objects — duplicate strings, duplicate primitive arrays, boxed primitives, and empty/singleton collection overhead. Fix the biggest category first for the highest impact. Figures are approximate._

| Waste Type                   |       Wasted |    Objects | Fix                                                                           |
| ---------------------------- | -----------: | ---------: | ----------------------------------------------------------------------------- |
| Boxed Primitives (footprint) |     169.0 KB |     10,689 | Use primitive arrays; or Eclipse Collections / Koloboke for typed collections |
| **Total**                    | **169.0 KB** | **10,689** |                                                                               |

## Top Retainers

_Combined ranking of `Class#field` references and stack-frame locals by retained heap. Retained totals can exceed heap size for linked structures (e.g. `List#next`) where each node retains its entire tail — treat as relative, not additive._

| Name                                                                          | Kind        | Retained |
| ----------------------------------------------------------------------------- | ----------- | -------: |
| cafesat.sat.Solver#initClauses()                                              | Stack Frame |   6.5 MB |
| cafesat.sat.Solver#solve()                                                    | Stack Frame |   4.6 MB |
| scala.collection.immutable.List#foreach()                                     | Stack Frame |   3.2 MB |
| cafesat.sat.Solver#$anonfun$initClauses$1()                                   | Stack Frame | 962.2 KB |
| cafesat.api.Solver$#solveForSatisfiability()                                  | Stack Frame | 208.8 KB |
| scala.collection.IterableOnceOps#exists()                                     | Stack Frame |    176 B |
| scala.collection.IterableOnceOps#count()                                      | Stack Frame |    160 B |
| java.lang.Thread#run()                                                        | Stack Frame |    152 B |
| java.lang.Thread#runWith()                                                    | Stack Frame |    152 B |
| java.util.concurrent.locks.AbstractQueuedSynchronizer$ConditionObject#await() | Stack Frame |    136 B |
| java.lang.ref.ReferenceQueue#remove0()                                        | Stack Frame |     88 B |
| java.lang.ref.NativeReferenceQueue#remove()                                   | Stack Frame |     56 B |
| java.lang.ref.ReferenceQueue#await()                                          | Stack Frame |     48 B |
| java.lang.ref.ReferenceQueue#remove()                                         | Stack Frame |     48 B |
| java.lang.ref.NativeReferenceQueue#await()                                    | Stack Frame |     40 B |
| java.lang.Object#wait()                                                       | Stack Frame |     32 B |
| scala.collection.AbstractIterable#exists()                                    | Stack Frame |     32 B |
| scala.collection.IterableOnceOps#exists$()                                    | Stack Frame |     32 B |
| java.util.concurrent.locks.LockSupport#parkNanos()                            | Stack Frame |     24 B |
| jdk.internal.ref.CleanerImpl#run()                                            | Stack Frame |     24 B |
| java.lang.ref.Finalizer$FinalizerThread#run()                                 | Stack Frame |     16 B |

## References

_Soft, weak, and phantom references — referents, retention status, and null-referent counts._

### Soft References

_Soft references keep objects alive until the JVM needs memory — cleared under GC pressure. A large soft-referenced heap signals an oversized cache; cap it with a max-entries limit or switch to an explicit bounded cache (e.g. Caffeine)._

_281 reference instances._

#### Referent Classes

| Class                                    | Objects | Shallow | Retained |
| ---------------------------------------- | ------: | ------: | -------: |
| `java.lang.invoke.LambdaForm`            |     178 |  8.3 KB |  77.3 KB |
| `java.lang.invoke.DirectMethodHandle`    |      34 |  1.3 KB |   1.3 KB |
| `java.lang.Class$ReflectionData`         |      21 |  1.3 KB |   1.3 KB |
| `sun.util.locale.BaseLocale`             |      20 |   640 B |   1.2 KB |
| `java.util.Locale`                       |      10 |   320 B |    320 B |
| `java.util.jar.Manifest`                 |       8 |   192 B |   1.1 MB |
| `java.util.concurrent.ConcurrentHashMap` |       4 |   256 B |   2.1 KB |
| `java.lang.Object[]`                     |       2 |    64 B |     64 B |
| `java.util.ArrayList`                    |       1 |    24 B |     80 B |
| `sun.text.resources.cldr.FormatData`     |       1 |    40 B |  28.3 KB |
| `sun.text.resources.cldr.FormatData_en`  |       1 |    40 B |  20.0 KB |
| `sun.util.resources.Bundles$1`           |       1 |    40 B |     40 B |

#### Only Weakly Retained

_Referents reachable only through soft references — no strong path. GC clears these under memory pressure._

| Class                            | Objects | Shallow | Retained |
| -------------------------------- | ------: | ------: | -------: |
| `java.lang.Class$ReflectionData` |      21 |  1.3 KB |   1.3 KB |

### Weak References

_Weak references let GC claim referents — reachable only via weak chains, reclaimed at any collection. Large counts are usually benign, but a growing count can indicate ThreadLocal leaks or listener registries not deregistering._

_975 reference instances. 23 instances have a null referent — referent collected, not yet processed._

#### Referent Classes

| Class                                                             | Objects | Shallow | Retained |
| ----------------------------------------------------------------- | ------: | ------: | -------: |
| `java.lang.invoke.MethodType`                                     |     894 | 34.9 KB | 118.7 KB |
| `java.lang.ClassValue$Identity`                                   |      11 |   176 B |    176 B |
| `java.util.logging.Level`                                         |       9 |   288 B |    288 B |
| `java.util.logging.Logger`                                        |       8 |   448 B |   4.3 KB |
| `java.lang.ClassValue$Version`                                    |       6 |   144 B |    336 B |
| `java.lang.Module`                                                |       4 |   192 B |  21.8 KB |
| `java.util.logging.LogManager$RootLogger`                         |       4 |   256 B |   1.4 KB |
| `sun.security.provider.FileInputStreamPool$UnclosableInputStream` |       2 |    32 B |     32 B |
| `java.lang.ClassLoader`                                           |       1 |    16 B |      0 B |
| `java.lang.ThreadGroup`                                           |       1 |    48 B |     48 B |
| `java.net.URLClassLoader`                                         |       1 |     8 B |      0 B |
| `java.security.Provider$Service`                                  |       1 |     8 B |      0 B |
| `java.security.SecureClassLoader`                                 |       1 |     0 B |      0 B |
| `jdk.internal.loader.BuiltinClassLoader`                          |       1 |    16 B |      0 B |
| `jdk.internal.loader.ClassLoaders$AppClassLoader`                 |       1 |     8 B |      0 B |
| `jdk.internal.loader.ClassLoaders$PlatformClassLoader`            |       1 |     8 B |      0 B |
| `jdk.internal.misc.TerminatingThreadLocal$1`                      |       1 |    16 B |     16 B |
| `org.renaissance.core.Launcher`                                   |       1 |    24 B |      0 B |
| `scala.reflect.ClassTag$GenericClassTag`                          |       1 |    16 B |     16 B |
| `scala.reflect.ManifestFactory$ObjectManifest`                    |       1 |    32 B |     32 B |
_… 2 more classes (2 objects, 32 B shallow, 32 B retained)._

#### Only Weakly Retained

_Referents reachable only through weak references — no strong or soft path. GC can reclaim them at any collection._

_None found — no objects are exclusively reachable via this reference kind._

### Phantom References

_Phantom references track objects in cleanup pipelines for native resource release. A large backlog signals a stalled or overloaded ReferenceQueue processor, or indicates native resources (file handles, off-heap buffers) not being released promptly._

_38 reference instances. 1 instance has a null referent — referent collected, not yet processed._

#### Referent Classes

| Class                                 | Objects | Shallow | Retained |
| ------------------------------------- | ------: | ------: | -------: |
| `java.io.FileDescriptor`              |      12 |   480 B |    480 B |
| `java.util.zip.Inflater`              |      11 |   704 B |    704 B |
| `java.util.jar.JarFile`               |      10 |   640 B |   1.2 MB |
| `java.lang.ref.Cleaner`               |       1 |    16 B |     16 B |
| `java.nio.DirectByteBuffer`           |       1 |    72 B |     72 B |
| `sun.net.www.protocol.jar.URLJarFile` |       1 |    80 B |    344 B |
| `sun.nio.fs.NativeBuffer`             |       1 |    32 B |     64 B |

#### Only Weakly Retained

_Referents reachable only through phantom references — queued for post-cleanup resource release._

_None found — no objects are exclusively reachable via this reference kind._

## Unreachable Objects

_4,266 unreachable objects, 673.0 KB shallow heap. Top 30 classes by shallow heap._

_Unreachable objects are eligible for collection but have not yet been reclaimed. A small unreachable heap (< 5% of heap total) is normal between GC cycles._

| Kind             | Objects |  Shallow |
| ---------------- | ------: | -------: |
| Instances        |   1,364 |  38.6 KB |
| Object Arrays    |      60 |   3.7 KB |
| Primitive Arrays |   2,726 | 630.7 KB |
| Class Objects    |     116 |      0 B |

_Shallow heap is additive; Retained sets overlap (nested subtrees are counted once per ancestor)._

| Class                                                                   | Objects |  Shallow | Retained |
| ----------------------------------------------------------------------- | ------: | -------: | -------: |
| `int[]`                                                                 |   1,642 | 569.6 KB | 569.6 KB |
| `byte[]`                                                                |   1,084 |  61.1 KB |  61.1 KB |
| `java.lang.String`                                                      |   1,084 |  25.4 KB |  86.5 KB |
| `java.lang.reflect.Field`                                               |      46 |   3.2 KB |   5.8 KB |
| `java.lang.ClassValue$Entry[]`                                          |      12 |   1.7 KB |   1.7 KB |
| `java.lang.reflect.Method`                                              |      18 |   1.5 KB |   3.2 KB |
| `java.lang.Class$ReflectionData`                                        |      21 |   1.3 KB |  13.1 KB |
| `java.util.WeakHashMap$Entry[]`                                         |      12 |    960 B |   1.4 KB |
| `java.lang.invoke.MemberName`                                           |      21 |    840 B |    904 B |
| `java.lang.ref.SoftReference`                                           |      21 |    840 B |  14.0 KB |
| `java.lang.ClassValue$ClassValueMap`                                    |      12 |    768 B |   5.4 KB |
| `java.lang.reflect.Constructor`                                         |       9 |    648 B |   2.0 KB |
| `java.util.WeakHashMap$Entry`                                           |      11 |    440 B |    440 B |
| `java.lang.ref.ReferenceQueue`                                          |      12 |    384 B |   1.2 KB |
| `java.util.concurrent.locks.ReentrantLock$NonfairSync`                  |      12 |    384 B |    384 B |
| `java.lang.invoke.DirectMethodHandle$Constructor`                       |       8 |    384 B |   1.1 KB |
| `java.util.concurrent.locks.AbstractQueuedSynchronizer$ConditionObject` |      12 |    288 B |    288 B |
| `java.lang.reflect.Field[]`                                             |       4 |    256 B |   6.1 KB |
| `java.lang.reflect.Method[]`                                            |      10 |    248 B |   3.4 KB |
| `java.lang.reflect.Constructor[]`                                       |      10 |    240 B |   2.3 KB |
| `java.util.HashMap$Node`                                                |       7 |    224 B |    224 B |
| `java.lang.Class[]`                                                     |       9 |    216 B |    216 B |
| `java.lang.Thread`                                                      |       2 |    208 B |    520 B |
| `java.lang.invoke.ResolvedMethodName`                                   |      12 |    192 B |    192 B |
| `java.util.concurrent.locks.ReentrantLock`                              |      12 |    192 B |    192 B |
| `jdk.internal.reflect.DirectConstructorHandleAccessor`                  |       8 |    192 B |   1.4 KB |
| `java.lang.ClassValue$Entry`                                            |       6 |    192 B |    352 B |
| `java.lang.ref.WeakReference`                                           |       5 |    160 B |    160 B |
| `java.lang.invoke.DirectMethodHandle`                                   |       4 |    160 B |    384 B |
| `java.lang.invoke.BoundMethodHandle$Species_L`                          |       4 |    160 B |    544 B |

### Garbage-Root Dominator Trees

_Top garbage-root subtrees by retained heap (unreachable objects with no reachable predecessor). Depth capped._

1. **int[]** — 512.0 KB (1 object in subtree)

2. **int[]** — 4.0 KB (1 object in subtree)

3. **java.lang.ref.SoftReference** — 3.2 KB (70 objects in subtree)
   └─ java.lang.Class$ReflectionData — 3.2 KB
        └─ java.lang.reflect.Field[] — 3.1 KB
             ├─ java.lang.reflect.Field — 200 B
             │    └─ jdk.internal.reflect.MethodHandleObjectFieldAccessorImpl — 128 B
             │         └─ java.lang.invoke.DirectMethodHandle$StaticAccessor — 96 B
             ├─ java.lang.reflect.Field — 144 B
             │    └─ java.lang.String — 72 B
             │         └─ byte[] — 48 B
             ├─ java.lang.reflect.Field — 144 B
             │    └─ java.lang.String — 72 B
             │         └─ byte[] — 48 B
             ├─ java.lang.reflect.Field — 136 B
             │    └─ java.lang.String — 64 B
             │         └─ byte[] — 40 B
             ├─ java.lang.reflect.Field — 136 B
             │    └─ java.lang.String — 64 B
             │         └─ byte[] — 40 B
             ├─ java.lang.reflect.Field — 136 B
             │    └─ java.lang.String — 64 B
             │         └─ byte[] — 40 B
             ├─ java.lang.reflect.Field — 136 B
             │    └─ java.lang.String — 64 B
             │         └─ byte[] — 40 B
             └─ java.lang.reflect.Field — 136 B
                  └─ java.lang.String — 64 B
                       └─ byte[] — 40 B

4. **int[]** — 3.1 KB (1 object in subtree)

5. **int[]** — 2.4 KB (1 object in subtree)

6. **int[]** — 2.4 KB (1 object in subtree)

7. **java.lang.ref.SoftReference** — 1.6 KB (36 objects in subtree)
   └─ java.lang.Class$ReflectionData — 1.6 KB
        └─ java.lang.reflect.Field[] — 1.5 KB
             ├─ java.lang.reflect.Field — 136 B
             │    └─ java.lang.String — 64 B
             │         └─ byte[] — 40 B
             ├─ java.lang.reflect.Field — 128 B
             │    └─ java.lang.String — 56 B
             │         └─ byte[] — 32 B
             ├─ java.lang.reflect.Field — 128 B
             │    └─ java.lang.String — 56 B
             │         └─ byte[] — 32 B
             ├─ java.lang.reflect.Field — 128 B
             │    └─ java.lang.String — 56 B
             │         └─ byte[] — 32 B
             ├─ java.lang.reflect.Field — 128 B
             │    └─ java.lang.String — 56 B
             │         └─ byte[] — 32 B
             ├─ java.lang.reflect.Field — 128 B
             │    └─ java.lang.String — 56 B
             │         └─ byte[] — 32 B
             ├─ java.lang.reflect.Field — 128 B
             │    └─ java.lang.String — 56 B
             │         └─ byte[] — 32 B
             └─ java.lang.reflect.Field — 120 B
                  └─ java.lang.String — 48 B
                       └─ byte[] — 24 B

8. **int[]** — 1.6 KB (1 object in subtree)

9. **java.lang.ref.SoftReference** — 1.1 KB (26 objects in subtree)
   └─ java.lang.Class$ReflectionData — 1.1 KB
        └─ java.lang.reflect.Field[] — 1016 B
             ├─ java.lang.reflect.Field — 200 B
             │    ├─ java.lang.String — 80 B
             │    │    └─ byte[] — 56 B
             │    └─ java.lang.String — 48 B
             │         └─ byte[] — 24 B
             ├─ java.lang.reflect.Field — 176 B
             │    ├─ java.lang.String — 56 B
             │    │    └─ byte[] — 32 B
             │    └─ java.lang.String — 48 B
             │         └─ byte[] — 24 B
             ├─ java.lang.reflect.Field — 136 B
             │    └─ java.lang.String — 64 B
             │         └─ byte[] — 40 B
             ├─ java.lang.reflect.Field — 136 B
             │    └─ java.lang.String — 64 B
             │         └─ byte[] — 40 B
             ├─ java.lang.reflect.Field — 128 B
             │    └─ java.lang.String — 56 B
             │         └─ byte[] — 32 B
             ├─ java.lang.reflect.Field — 120 B
             │    └─ java.lang.String — 48 B
             │         └─ byte[] — 24 B
             └─ java.lang.reflect.Field — 72 B

10. **java.lang.ref.SoftReference** — 912 B (19 objects in subtree)
   └─ java.lang.Class$ReflectionData — 872 B
        ├─ java.lang.reflect.Method[] — 544 B
        │    ├─ java.lang.reflect.Method — 304 B
        │    │    ├─ java.lang.String — 120 B
        │    │    │    └─ byte[] — 96 B
        │    │    ├─ java.lang.String — 72 B
        │    │    │    └─ byte[] — 48 B
        │    │    └─ java.lang.Class[] — 24 B
        │    └─ java.lang.reflect.Method — 216 B
        │         └─ java.lang.String — 128 B
        │              └─ byte[] — 104 B
        └─ java.lang.reflect.Constructor[] — 264 B
             └─ java.lang.reflect.Constructor — 240 B
                  └─ jdk.internal.reflect.DirectConstructorHandleAccessor — 168 B
                       └─ java.lang.invoke.DirectMethodHandle$Constructor — 144 B

## Allocation Sites

_Objects grouped by the stack trace that allocated them — shows where heap was created, not necessarily what is keeping it alive. Only available when the dump was captured with the HPROF agent (JDK 8 and earlier). Each site is a candidate to allocate less by pooling, caching, or deferring construction._

_Allocation-site records are present but contain no per-frame data. The HPROF agent must be invoked with `depth=8` or higher to record method-level allocation stacks: `-agentlib:hprof=heap=dump,depth=8`._

## Retention Concentration

_Share of the reachable heap retained by the few largest top-level dominators (a dominator's retained size is everything it keeps alive). Read it as a concentration curve: if **Top 1** is already high, one object is the accumulation point — making it unreachable reclaims most of the heap; if the share only climbs as you widen to **Top 10** / **Top 100**, retention is spread across many peers (e.g. a big cache or collection of similar objects) and no single fix helps much._

| Scope           | Retained Share | Retained |
| --------------- | -------------: | -------: |
| Top 1 object    |          76.7% |  22.9 MB |
| Top 10 objects  |          92.6% |  27.6 MB |
| Top 100 objects |          96.4% |  28.7 MB |

_4 objects each hold ≥1% of the reachable heap._

## Dominator-Depth Distribution

_How many dominator hops each object sits below a GC root. A spike at depth 1–3 is normal; a long tail at depth 10+ points to deeply nested containers or linked structures._

_Half of all live objects sit within 10 hops of a GC root; the deepest chain is 41355 hops._

| Depth | Objects | % Objects | Cumulative % |
| ----: | ------: | --------: | -----------: |
|     1 |  13,474 |      1.4% |         1.4% |
|     2 |  44,435 |      4.7% |         6.1% |
|     3 | 157,764 |     16.6% |        22.6% |
|     4 |  64,998 |      6.8% |        29.5% |
|     5 |  65,480 |      6.9% |        36.3% |
|     6 |  30,516 |      3.2% |        39.5% |
|     7 |  16,550 |      1.7% |        41.3% |
|     8 |  22,589 |      2.4% |        43.6% |
|     9 |  33,631 |      3.5% |        47.2% |
|    10 |  58,250 |      6.1% |        53.3% |
|    11 |  42,886 |      4.5% |        57.8% |
|    12 |  90,772 |      9.5% |        67.3% |
|    13 |   3,095 |      0.3% |        67.6% |
|    14 |   6,403 |      0.7% |        68.3% |
|    15 |     338 |     <0.1% |        68.4% |
|    16 |   1,417 |      0.1% |        68.5% |
|    17 |   2,103 |      0.2% |        68.7% |
|    18 |   7,873 |      0.8% |        69.5% |
|    19 |   2,054 |      0.2% |        69.8% |
|    20 |   2,128 |      0.2% |        70.0% |

_… (+41335 deeper buckets, 285,909 objects, 100.0% cumulative — full data in JSON)_

## Leak Indicators

_Point-in-time counts for known Java leak patterns. Non-zero values are not always bugs — see the **What to Check** column for how to triage each one._

| Indicator                            |    Value | What to Check                                                                                                                                         |
| ------------------------------------ | -------: | ----------------------------------------------------------------------------------------------------------------------------------------------------- |
| Anonymous/generated classes          |      178 | High counts signal class-loader leaks (e.g. dynamic proxies accumulating per request). In Top Consumers, filter by `$` to find the biggest offenders. |
| `DirectByteBuffer` off-heap capacity | 134.3 MB | Native memory, excluded from JVM heap totals. Check for NIO buffer pools that leak on close, or Netty/gRPC allocators missing a buffer cap.           |

## Glossary

_Definitions for the heap analysis terms used throughout this report._

- **Shallow size**: the memory an object occupies by itself, meaning its header
  plus its own fields (and, for an array, its elements). It does *not* include the
  objects it points to.
- **Retained heap (retained size)**: the total memory that would be reclaimed if this
  object became unreachable — its own shallow size plus everything
  reachable *only* through it. This is the number that answers "how much would
  making it unreachable reclaim?" and it is the basis for every percentage in this
  report. See [dominator (graph theory)](https://en.wikipedia.org/wiki/Dominator_(graph_theory)).
- **Reachable heap**: all objects the [garbage collector](https://en.wikipedia.org/wiki/Garbage_collection_(computer_science)) can still
  reach from a GC root. Anything unreachable is already collectible and is excluded
  from the totals here.
- **GC root**: an object the JVM keeps alive unconditionally, such as live thread
  stacks (local variables), static fields of loaded classes,
  [JNI](https://en.wikipedia.org/wiki/Java_Native_Interface) references, and
  similar. Every retained-size chain ends at a GC root.
- **Dominator**: object *A* dominates object *B* if every path from a GC root to
  *B* passes through *A*. In other words, if *A* became unreachable, *B* would become
  unreachable too. An object's retained heap is exactly the set of objects it
  dominates. See [dominator (graph theory)](https://en.wikipedia.org/wiki/Dominator_(graph_theory)).
- **Dominator tree**: the tree formed by linking each object to its immediate
  dominator. Retained sizes are computed by summing shallow sizes up this tree.
- **Top-level dominator**: an object whose immediate dominator is a GC root, so it
  sits at the top of the dominator tree. The "Biggest Objects" and "Retention
  Concentration" views rank these.
- **Dominator depth**: how many dominator-tree hops an object sits below a GC root.
  Shallow depth means most objects are held close to a root; deep depth means
  retention flows through long chains (nested collections, linked lists).
- **Accumulation point**: a single object (often a collection, cache, or map) that
  dominates a large number of instances of the *same* class, meaning where a
  [memory leak](https://en.wikipedia.org/wiki/Memory_leak) accumulates.
- **Class loader**: the JVM component that defined a class. The same class name
  loaded by two different [class loaders](https://en.wikipedia.org/wiki/Java_Classloader)
  is two distinct classes in the heap, so heap is attributed per (class, loader)
  pair.
- **Referent**: the object that a reference field points *to*. A
  [`WeakReference`](https://en.wikipedia.org/wiki/Weak_reference), for example, has
  a referent it does not keep alive.
- **Instance vs. class**: an *instance* is one object; a *class* row aggregates
  every instance of that type. "Largest" in the histogram is the shallow size of
  the single biggest instance of a class.
- **Collection fill ratio**: the fraction of a collection's backing-array capacity
  that is actually occupied by elements — `elements / capacity`. A fill ratio near
  0 means the backing array is mostly empty (wasted memory). A ratio near 1 means
  the collection is full.
- **Map Load Factor**: for hash maps, the fraction of backing-array
  slots occupied — `occupied_slots / capacity`. A low load factor means many
  empty buckets (wasted memory); a high load factor (≥ 90%) increases hash
  collision chains and lookup cost.
- **Only-weakly retained**: an object that has no incoming strong reference — it is
  reachable only through one or more `WeakReference`, `SoftReference`, or
  `PhantomReference` chains. Weak-only referents are collected at the next GC cycle;
  soft-only referents are collected under memory pressure; phantom-only referents are
  already unreachable and queued for resource cleanup.
- **Compressed OOPs** (Compressed Ordinary Object Pointers): a JVM optimisation
  where object references are stored as 32-bit integers instead of 64-bit pointers,
  halving reference-field overhead on heaps <= ~32 GB. Visible in the Heap Summary
  as `Compressed OOPs: yes`.
- **Class#field**: the notation used throughout this report to identify a specific
  field — `HolderClass#fieldName`. For example `java.util.HashMap#table` names the
  `table` field of `HashMap`. This is the dominant incoming reference path for an
  object, not a guaranteed allocation site — it is a hint, not a precise origin.
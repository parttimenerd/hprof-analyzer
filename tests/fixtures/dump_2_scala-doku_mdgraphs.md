# Heap Dump Analysis: `dump_2_scala-doku.hprof`


_All sizes are binary (1 KB = 1024 bytes, 1 MB = 1024 KB, and so on)._

----

## Contents

- [Summary](#summary)
- [OOM Triage](#oom-triage)
- [System Overview](#system-overview)
- [Leak Suspects](#leak-suspects)
- [Top Consumers](#top-consumers)
- [Dominator Analysis](#dominator-analysis)
- [Threads](#threads)
- [Top Components](#top-components)
- [Arrays by Size](#arrays-by-size)
- [Collections](#collections)
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

|  # | Suspect                                                       | Retained | % Heap |
| -: | ------------------------------------------------------------- | -------: | -----: |
|  1 | `cafesat.sat.Vector` (36,614 instances)                       |   4.2 MB |  14.1% |
|  2 | `scala.collection.immutable.$colon$colon` (146,148 instances) |   3.4 MB |  11.3% |

**Likely problem:** retention is spread across several roots; no single object dominates.

## Memory Triage

_Where the reachable heap is concentrated, at a glance._

- **Headline retainer:** `cafesat.sat.Vector` (a class group) retains 4.2 MB (14.1% of reachable heap). See [Leak Suspects](#leak-suspects).
- **Concentration:** diffuse — retention is spread across multiple roots, so there is no single object to free. See [Leak Suspects](#leak-suspects).
- **Shape:** deep (retention flows through long dominator chains — often nested collections or linked structures) — 90% of objects within depth 6, max depth 16. See [Dominator-Depth Distribution](#dominator-depth-distribution).
- **One leak or many:** the single biggest object, `java.net.URLClassLoader`, retains 8.8% and the top 10 retain 13.1% of the heap; 1 object(s) each hold >=1%. See [Top Consumers](#top-consumers).
- **Classloader reload (low count):** `scala.collection.immutable.$colon$colon` is loaded by 2 class loaders (3.4 MB retained) — possible reload, but count is low; investigate only if count grows. See [Duplicate Classes](#duplicate-classes).
- **Off-heap (DirectByteBuffer):** 134.3 MB of native memory is held by live DirectByteBuffers — not counted in heap size but can dominate RSS. See [Leak Indicators](#leak-indicators).
- **Sparse object arrays:** 38,119 object arrays are <=20% full (5.9 MB wasted on null slots) — sparse or multi-dimensional array structures consuming excess memory. See [Collections](#collections).
- **Fixed per-object header overhead:** 952,666 objects × 12 B header = 10.9 MB (36.6% of heap) is consumed by JVM object headers alone — consider value types, primitive arrays, or fewer wrapper objects. See [Header Overhead](#header-overhead).
- **Empty-collection cemetery:** 5,806 of 6,307 tracked collections (92.1%) are empty (size == 0) — pre-allocated but never populated containers waste object-header overhead; consider lazy initialisation or null. See [Collections](#collections).
- **Collection waste not analyzed:** _Collection waste not analyzed — re-run with `--collections` to check for wasted capacity._

## System Overview

_Reachable-heap totals and the largest classes by retained heap._

### Heap Summary

| Property                         | Value                            |
| -------------------------------- | -------------------------------- |
| HPROF format                     | JAVA PROFILE 1.0.2               |
| File size                        | 50.5 MB                          |
| Identifier size                  | 64-bit                           |
| Compressed OOPs                  | yes                              |
| Dump created                     | 2026-07-08T12:44:31Z             |
| Total objects                    | 952,666                          |
| Total reachable heap             | 29.8 MB                          |
| Off-heap / on-heap               | 134.3 MB off-heap (4.5× on-heap) |
| GC roots                         | 1,509                            |
| Classes loaded                   | 2,851                            |
| Class loaders                    | 5                                |
| Unreachable objects (excluded)   | 4,266 (673.0 KB)                 |
| Heap fragmentation               | 2.2%                             |
| Top-class retained concentration | 16.0%                            |

- **Class loaders (labels):** java/net/URLClassLoader, jdk/internal/loader/ClassLoaders$AppClassLoader, jdk/internal/loader/ClassLoaders$PlatformClassLoader

### GC Roots by Type

| Root Type    | Count |                  |
| ------------ | ----: | ---------------- |
| Sticky Class | 1,402 | ████████████████ |
| JNI Global   |   100 | █▏               |
| Thread       |     7 | ▏                |

### Heap Composition

| Kind             | Objects | Shallow Heap |                  |
| ---------------- | ------: | -----------: | ---------------- |
| Instances        | 770,497 |      19.7 MB | ████████████████ |
| Object arrays    |  65,061 |       5.4 MB | ████▍            |
| Primitive arrays | 114,257 |       4.7 MB | ███▊             |
| Class objects    |   2,851 |      34.9 KB | ▏                |

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

### Class Histogram (by Retained Heap)

_Top 50 classes ranked by retained heap; the full list is in the JSON output._

|  # | Class                                             | Instances | Shallow Heap |  Largest | Retained Heap | % Heap |                  |
| -: | ------------------------------------------------- | --------: | -----------: | -------: | ------------: | -----: | ---------------- |
|  1 | `cafesat.sat.Literal`                             |   125,219 |       4.8 MB |     40 B |        4.8 MB |  16.0% | ████████████████ |
|  2 | `java.lang.Object[]`                              |    25,579 |       1.5 MB | 512.0 KB |        4.5 MB |  15.0% | ██████████████▉  |
|  3 | `cafesat.sat.Vector`                              |    36,614 |     858.1 KB |     24 B |        4.2 MB |  14.1% | ██████████████   |
|  4 | `cafesat.sat.Solver$Clause[]`                     |    36,615 |       3.4 MB |  71.5 KB |        3.4 MB |  11.5% | ███████████▍     |
|  5 | `scala.collection.immutable.$colon$colon`         |   146,151 |       3.3 MB |     24 B |        3.4 MB |  11.3% | ███████████▎     |
|  6 | `java.lang.Class`                                 |     2,860 |      35.5 KB |   1.1 KB |        2.8 MB |   9.3% | █████████▏       |
|  7 | `java.net.URLClassLoader`                         |         2 |        176 B |     88 B |        2.7 MB |   9.2% | █████████▏       |
|  8 | `scala.collection.immutable.Set$Set2`             |    44,628 |       1.0 MB |     24 B |        2.7 MB |   9.1% | █████████        |
|  9 | `int[]`                                           |    89,265 |       2.6 MB |  71.5 KB |        2.6 MB |   8.8% | ████████▊        |
| 10 | `java.util.ArrayList`                             |       102 |       2.4 KB |     24 B |        2.6 MB |   8.7% | ████████▋        |
| 11 | `scala.runtime.LazyVals$`                         |         1 |         16 B |     16 B |        2.5 MB |   8.4% | ████████▍        |
| 12 | `java.lang.Object`                                |   133,780 |       2.0 MB |     16 B |        2.0 MB |   6.8% | ██████▊          |
| 13 | `byte[]`                                          |    24,721 |       2.0 MB | 255.1 KB |        2.0 MB |   6.7% | ██████▋          |
| 14 | `cafesat.sat.Solver$Clause`                       |    65,565 |       2.0 MB |     32 B |        2.0 MB |   6.7% | ██████▋          |
| 15 | `scala.collection.immutable.BitmapIndexedSetNode` |    22,791 |     890.3 KB |     40 B |        1.5 MB |   4.9% | ████▊            |
| 16 | `cafesat.asts.core.Trees$ConnectiveApplication`   |    43,982 |       1.0 MB |     24 B |        1.0 MB |   3.4% | ███▎             |
| 17 | `java.lang.String`                                |    23,997 |     562.4 KB |     24 B |      847.8 KB |   2.8% | ██▊              |
| 18 | `cafesat.asts.core.Trees$ConnectiveSymbol`        |    35,234 |     825.8 KB |     24 B |      825.9 KB |   2.7% | ██▋              |
| 19 | `scala.collection.immutable.Set$Set3`             |     8,748 |     205.0 KB |     24 B |      546.7 KB |   1.8% | █▊               |
| 20 | `cafesat.asts.core.Trees$PredicateApplication`    |    19,761 |     463.1 KB |     24 B |      463.2 KB |   1.5% | █▌               |
| 21 | `java.util.HashMap$Node`                          |    10,167 |     317.7 KB |     32 B |      429.7 KB |   1.4% | █▍               |
| 22 | `java.util.concurrent.ConcurrentHashMap$Node`     |     7,007 |     219.0 KB |     32 B |      375.1 KB |   1.2% | █▏               |
| 23 | `java.util.LinkedHashMap`                         |     5,826 |     364.1 KB |     64 B |      364.2 KB |   1.2% | █▏               |
| 24 | `cafesat.api.Formulas$Formula`                    |     8,908 |     139.2 KB |     16 B |      347.9 KB |   1.1% | █▏               |
| 25 | `java.util.zip.ZipFile$Source`                    |        10 |        800 B |     80 B |      263.5 KB |   0.9% | ▊                |
| 26 | `java.util.concurrent.ConcurrentHashMap$Node[]`   |        93 |      61.9 KB |   8.0 KB |      239.4 KB |   0.8% | ▊                |
| 27 | `java.util.HashMap$Node[]`                        |       395 |      96.5 KB |  16.0 KB |      174.8 KB |   0.6% | ▌                |
| 28 | `java.lang.Integer`                               |     9,789 |     153.0 KB |     16 B |      153.4 KB |   0.5% | ▌                |
| 29 | `cafesat.sat.Vector[]`                            |         1 |     143.0 KB | 143.0 KB |      143.0 KB |   0.5% | ▍                |
| 30 | `java.util.concurrent.ConcurrentHashMap`          |       117 |       7.3 KB |     64 B |      135.8 KB |   0.4% | ▍                |
| 31 | `byte[][]`                                        |         1 |       1.4 KB |   1.4 KB |       94.1 KB |   0.3% | ▎                |
| 32 | `java.util.jar.Attributes`                        |     5,786 |      90.4 KB |     16 B |       90.5 KB |   0.3% | ▎                |
| 33 | `scala.collection.immutable.HashSet`              |        85 |       1.3 KB |     16 B |       82.9 KB |   0.3% | ▎                |
| 34 | `java.util.HashMap`                               |       361 |      16.9 KB |     48 B |       80.5 KB |   0.3% | ▎                |
| 35 | `cafesat.sat.Solver`                              |         1 |        168 B |    168 B |       72.4 KB |   0.2% | ▏                |
| 36 | `char[]`                                          |       224 |      55.8 KB |  16.0 KB |       55.8 KB |   0.2% | ▏                |
| 37 | `java.util.LinkedHashMap$Entry`                   |     1,200 |      46.9 KB |     40 B |       46.9 KB |   0.2% | ▏                |
| 38 | `java.lang.invoke.MemberName`                     |     1,040 |      40.6 KB |     40 B |       44.7 KB |   0.1% | ▏                |
| 39 | `java.lang.Thread`                                |        27 |       2.7 KB |    104 B |       38.4 KB |   0.1% | ▏                |
| 40 | `java.lang.invoke.MethodType`                     |       894 |      34.9 KB |     40 B |       38.4 KB |   0.1% | ▏                |
| 41 | `cafesat.api.Formulas$Formula[]`                  |         2 |      35.1 KB |  34.8 KB |       35.1 KB |   0.1% | ▏                |
| 42 | `jdk.internal.math.FDBigInteger`                  |       341 |      10.7 KB |     32 B |       35.0 KB |   0.1% | ▏                |
| 43 | `java.lang.ref.SoftReference[]`                   |       434 |      32.2 KB |    120 B |       32.6 KB |   0.1% | ▏                |
| 44 | `java.lang.CharacterData00`                       |         1 |         16 B |     16 B |       29.8 KB |   0.1% | ▏                |
| 45 | `jdk.internal.util.WeakReferenceKey`              |       900 |      28.1 KB |     32 B |       29.4 KB |   0.1% | ▏                |
| 46 | `java.lang.Object[][]`                            |        13 |       1.3 KB |    144 B |       28.8 KB |   0.1% | ▏                |
| 47 | `org.renaissance.core.BenchmarkDescriptor`        |        31 |        744 B |     24 B |       28.2 KB |   0.1% | ▏                |
| 48 | `java.lang.String[]`                              |       449 |      17.0 KB |   2.4 KB |       27.8 KB |   0.1% | ▏                |
| 49 | `java.lang.Module`                                |        70 |       3.3 KB |     48 B |       25.7 KB |   0.1% | ▏                |
| 50 | `java.lang.invoke.LambdaForm$Name`                |       503 |      15.7 KB |     32 B |       25.4 KB |   0.1% | ▏                |

### Class Loaders

_Classes grouped by the loader that defined them; many loaders each holding heap can signal a class-loader leak._

| Loader                                               | Classes | Instances | Shallow Heap | Retained Heap |                  |
| ---------------------------------------------------- | ------: | --------: | -----------: | ------------: | ---------------- |
| java/net/URLClassLoader                              |     606 |   596,452 |      19.1 MB |       25.5 MB | ████████████████ |
| <boot>                                               |   1,703 |   355,819 |      10.7 MB |       23.4 MB | ██████████████▋  |
| java/net/URLClassLoader                              |     575 |       330 |      18.0 KB |        2.6 MB | █▌               |
| jdk/internal/loader/ClassLoaders$AppClassLoader      |      82 |        64 |       1.3 KB |       30.0 KB | ▏                |
| jdk/internal/loader/ClassLoaders$PlatformClassLoader |       1 |         1 |         16 B |          24 B | ▏                |

### Duplicate Classes

_Class names loaded by more than one class loader — a classic class-loader-leak signature (the same class re-loaded repeatedly)._

| Class                                      | #Loaders | Instances | Retained Heap |
| ------------------------------------------ | -------: | --------: | ------------: |
| `scala.collection.immutable.$colon$colon`  |        2 |   146,181 |        3.4 MB |
| `scala.math.BigInt[]`                      |        2 |         2 |       16.0 KB |
| `scala.math.BigInt$`                       |        2 |         2 |        8.2 KB |
| `scala.Option[]`                           |        2 |        18 |        3.5 KB |
| `scala.Some`                               |        2 |       176 |        2.8 KB |
| `scala.collection.immutable.LazyList`      |        2 |         2 |        1.5 KB |
| `scala.collection.mutable.HashMap`         |        2 |         4 |         752 B |
| `scala.collection.immutable.LazyList$`     |        2 |         2 |         688 B |
| `scala.collection.IterableOps`             |        2 |         0 |         608 B |
| `scala.collection.mutable.HashMap$Node`    |        2 |        11 |         576 B |
| `scala.Array$`                             |        2 |         2 |         480 B |
| `scala.collection.ArrayOps$`               |        2 |         2 |         464 B |
| `scala.collection.IterableOnceOps`         |        2 |         0 |         448 B |
| `scala.collection.mutable.Buffer`          |        2 |         0 |         384 B |
| `scala.collection.Iterator`                |        2 |         0 |         368 B |
| `scala.collection.SeqOps`                  |        2 |         0 |         352 B |
| `scala.collection.ClassTagIterableFactory` |        2 |         0 |         336 B |
| `scala.collection.IterableFactory`         |        2 |         0 |         336 B |
| `scala.runtime.ScalaRunTime$`              |        2 |         2 |         336 B |
| `scala.collection.LinearSeq`               |        2 |         0 |         320 B |

**`scala.collection.immutable.$colon$colon`** — per loader:

| Loader                                | Instances | Shallow | Retained Heap |                  |
| ------------------------------------- | --------: | ------: | ------------: | ---------------- |
| `java/net/URLClassLoader` @0xc0412288 |   146,151 |  3.3 MB |        3.4 MB | ████████████████ |
| `java/net/URLClassLoader` @0xce800048 |        30 |   720 B |         856 B | ▏                |

**`scala.math.BigInt[]`** — per loader:

| Loader                                | Instances | Shallow | Retained Heap |                  |
| ------------------------------------- | --------: | ------: | ------------: | ---------------- |
| `java/net/URLClassLoader` @0xc0412288 |         1 |  8.0 KB |        8.0 KB | ████████████████ |
| `java/net/URLClassLoader` @0xce800048 |         1 |  8.0 KB |        8.0 KB | ████████████████ |

**`scala.math.BigInt$`** — per loader:

| Loader                                | Instances | Shallow | Retained Heap |                  |
| ------------------------------------- | --------: | ------: | ------------: | ---------------- |
| `java/net/URLClassLoader` @0xce800048 |         1 |    16 B |        8.1 KB | ████████████████ |
| `java/net/URLClassLoader` @0xc0412288 |         1 |    16 B |         112 B | ▏                |

**`scala.Option[]`** — per loader:

| Loader                                | Instances | Shallow | Retained Heap |                  |
| ------------------------------------- | --------: | ------: | ------------: | ---------------- |
| `java/net/URLClassLoader` @0xc0412288 |        18 |  1008 B |        3.5 KB | ████████████████ |
| `java/net/URLClassLoader` @0xce800048 |         0 |     0 B |           0 B |                  |

**`scala.Some`** — per loader:

| Loader                                | Instances | Shallow | Retained Heap |                  |
| ------------------------------------- | --------: | ------: | ------------: | ---------------- |
| `java/net/URLClassLoader` @0xc0412288 |       158 |  2.5 KB |        2.5 KB | ████████████████ |
| `java/net/URLClassLoader` @0xce800048 |        18 |   288 B |         328 B | ██               |

**`scala.collection.immutable.LazyList`** — per loader:

| Loader                                | Instances | Shallow | Retained Heap |                  |
| ------------------------------------- | --------: | ------: | ------------: | ---------------- |
| `java/net/URLClassLoader` @0xc0412288 |         1 |    24 B |         744 B | ████████████████ |
| `java/net/URLClassLoader` @0xce800048 |         1 |    24 B |         744 B | ████████████████ |

**`scala.collection.mutable.HashMap`** — per loader:

| Loader                                | Instances | Shallow | Retained Heap |                  |
| ------------------------------------- | --------: | ------: | ------------: | ---------------- |
| `java/net/URLClassLoader` @0xc0412288 |         1 |    32 B |         408 B | ████████████████ |
| `java/net/URLClassLoader` @0xce800048 |         3 |    96 B |         344 B | █████████████▍   |

**`scala.collection.immutable.LazyList$`** — per loader:

| Loader                                | Instances | Shallow | Retained Heap |                  |
| ------------------------------------- | --------: | ------: | ------------: | ---------------- |
| `java/net/URLClassLoader` @0xce800048 |         1 |    16 B |         360 B | ████████████████ |
| `java/net/URLClassLoader` @0xc0412288 |         1 |    16 B |         328 B | ██████████████▌  |

**`scala.collection.IterableOps`** — per loader:

| Loader                                | Instances | Shallow | Retained Heap |                  |
| ------------------------------------- | --------: | ------: | ------------: | ---------------- |
| `java/net/URLClassLoader` @0xc0412288 |         0 |     0 B |         304 B | ████████████████ |
| `java/net/URLClassLoader` @0xce800048 |         0 |     0 B |         304 B | ████████████████ |

**`scala.collection.mutable.HashMap$Node`** — per loader:

| Loader                                | Instances | Shallow | Retained Heap |                  |
| ------------------------------------- | --------: | ------: | ------------: | ---------------- |
| `java/net/URLClassLoader` @0xc0412288 |         7 |   224 B |         408 B | ████████████████ |
| `java/net/URLClassLoader` @0xce800048 |         4 |   128 B |         168 B | ██████▌          |

**`scala.Array$`** — per loader:

| Loader                                | Instances | Shallow | Retained Heap |                  |
| ------------------------------------- | --------: | ------: | ------------: | ---------------- |
| `java/net/URLClassLoader` @0xce800048 |         1 |    16 B |         248 B | ████████████████ |
| `java/net/URLClassLoader` @0xc0412288 |         1 |    16 B |         232 B | ██████████████▉  |

**`scala.collection.ArrayOps$`** — per loader:

| Loader                                | Instances | Shallow | Retained Heap |                  |
| ------------------------------------- | --------: | ------: | ------------: | ---------------- |
| `java/net/URLClassLoader` @0xc0412288 |         1 |    16 B |         232 B | ████████████████ |
| `java/net/URLClassLoader` @0xce800048 |         1 |    16 B |         232 B | ████████████████ |

**`scala.collection.IterableOnceOps`** — per loader:

| Loader                                | Instances | Shallow | Retained Heap |                  |
| ------------------------------------- | --------: | ------: | ------------: | ---------------- |
| `java/net/URLClassLoader` @0xc0412288 |         0 |     0 B |         224 B | ████████████████ |
| `java/net/URLClassLoader` @0xce800048 |         0 |     0 B |         224 B | ████████████████ |

**`scala.collection.mutable.Buffer`** — per loader:

| Loader                                | Instances | Shallow | Retained Heap |                  |
| ------------------------------------- | --------: | ------: | ------------: | ---------------- |
| `java/net/URLClassLoader` @0xc0412288 |         0 |     0 B |         192 B | ████████████████ |
| `java/net/URLClassLoader` @0xce800048 |         0 |     0 B |         192 B | ████████████████ |

**`scala.collection.Iterator`** — per loader:

| Loader                                | Instances | Shallow | Retained Heap |                  |
| ------------------------------------- | --------: | ------: | ------------: | ---------------- |
| `java/net/URLClassLoader` @0xc0412288 |         0 |     0 B |         184 B | ████████████████ |
| `java/net/URLClassLoader` @0xce800048 |         0 |     0 B |         184 B | ████████████████ |

**`scala.collection.SeqOps`** — per loader:

| Loader                                | Instances | Shallow | Retained Heap |                  |
| ------------------------------------- | --------: | ------: | ------------: | ---------------- |
| `java/net/URLClassLoader` @0xc0412288 |         0 |     0 B |         176 B | ████████████████ |
| `java/net/URLClassLoader` @0xce800048 |         0 |     0 B |         176 B | ████████████████ |

**`scala.collection.ClassTagIterableFactory`** — per loader:

| Loader                                | Instances | Shallow | Retained Heap |                  |
| ------------------------------------- | --------: | ------: | ------------: | ---------------- |
| `java/net/URLClassLoader` @0xc0412288 |         0 |     0 B |         168 B | ████████████████ |
| `java/net/URLClassLoader` @0xce800048 |         0 |     0 B |         168 B | ████████████████ |

**`scala.collection.IterableFactory`** — per loader:

| Loader                                | Instances | Shallow | Retained Heap |                  |
| ------------------------------------- | --------: | ------: | ------------: | ---------------- |
| `java/net/URLClassLoader` @0xc0412288 |         0 |     0 B |         168 B | ████████████████ |
| `java/net/URLClassLoader` @0xce800048 |         0 |     0 B |         168 B | ████████████████ |

**`scala.runtime.ScalaRunTime$`** — per loader:

| Loader                                | Instances | Shallow | Retained Heap |                  |
| ------------------------------------- | --------: | ------: | ------------: | ---------------- |
| `java/net/URLClassLoader` @0xc0412288 |         1 |    16 B |         168 B | ████████████████ |
| `java/net/URLClassLoader` @0xce800048 |         1 |    16 B |         168 B | ████████████████ |

**`scala.collection.LinearSeq`** — per loader:

| Loader                                | Instances | Shallow | Retained Heap |                  |
| ------------------------------------- | --------: | ------: | ------------: | ---------------- |
| `java/net/URLClassLoader` @0xc0412288 |         0 |     0 B |         160 B | ████████████████ |
| `java/net/URLClassLoader` @0xce800048 |         0 |     0 B |         160 B | ████████████████ |

## Leak Suspects

_Objects and class groups whose retained heap is large enough to be a likely OOM cause, ranked by retained heap._

|  # | Suspect                                   | Retained | % Heap |                  |
| -: | ----------------------------------------- | -------: | -----: | ---------------- |
|  1 | `cafesat.sat.Vector`                      |   4.2 MB |  14.1% | ████████████████ |
|  2 | `scala.collection.immutable.$colon$colon` |   3.4 MB |  11.3% | ████████████▊    |

### 1. `cafesat.sat.Vector` — retains 4.2 MB (14.1% of reachable heap)

36,614 instances of `cafesat.sat.Vector` together retain this heap (combined shallow 858.1 KB).

#### Merged Paths to GC Roots

```
cafesat.sat.Vector (36,614 objects, retained 4.2 MB)
└─ cafesat.sat.Vector (36,614 objects, retained 4.2 MB)
```

### 2. `scala.collection.immutable.$colon$colon` — retains 3.4 MB (11.3% of reachable heap)

146,148 instances of `scala.collection.immutable.$colon$colon` together retain this heap (combined shallow 3.3 MB).

#### Merged Paths to GC Roots

```
scala.collection.immutable.$colon$colon (146,148 objects, retained 3.4 MB)
└─ scala.collection.immutable.$colon$colon (146,148 objects, retained 3.4 MB)
```

## Top Consumers

### Biggest Objects (Top-Level Dominators)

_Individual objects retaining the most heap; `% Heap` is the share of total reachable heap._

|  # | Class                                             |  Shallow | Retained | % Heap |                  |
| -: | ------------------------------------------------- | -------: | -------: | -----: | ---------------- |
|  1 | `java.net.URLClassLoader`                         |     88 B |   2.6 MB |   8.8% | ████████████████ |
|  2 | `java.util.zip.ZipFile$Source`                    |     80 B | 255.2 KB |   0.8% | █▌               |
|  3 | `byte[]`                                          | 255.1 KB | 255.1 KB |   0.8% | █▌               |
|  4 | `scala.collection.immutable.BitmapIndexedSetNode` |     40 B | 246.4 KB |   0.8% | █▍               |
|  5 | `cafesat.sat.Vector[]`                            | 143.0 KB | 143.0 KB |   0.5% | ▊                |
|  6 | `java.net.URLClassLoader`                         |     88 B | 129.0 KB |   0.4% | ▊                |
|  7 | `byte[][]`                                        |   1.4 KB |  94.1 KB |   0.3% | ▌                |
|  8 | `java.util.concurrent.ConcurrentHashMap`          |     64 B |  73.3 KB |   0.2% | ▍                |
|  9 | `cafesat.sat.Solver`                              |    168 B |  71.7 KB |   0.2% | ▍                |
| 10 | `int[]`                                           |  71.5 KB |  71.5 KB |   0.2% | ▍                |
| 11 | `cafesat.sat.Solver$Clause[]`                     |  71.5 KB |  71.5 KB |   0.2% | ▍                |
| 12 | `int[]`                                           |  71.5 KB |  71.5 KB |   0.2% | ▍                |
| 13 | `byte[]`                                          |  58.8 KB |  58.8 KB |   0.2% | ▎                |
| 14 | `scala.collection.immutable.HashSet`              |     16 B |  57.9 KB |   0.2% | ▎                |
| 15 | `byte[]`                                          |  54.0 KB |  54.0 KB |   0.2% | ▎                |
| 16 | `byte[]`                                          |  45.1 KB |  45.1 KB |   0.1% | ▎                |
| 17 | `java.lang.Object[]`                              |   8.9 KB |  35.8 KB |   0.1% | ▏                |
| 18 | `java.lang.Thread`                                |    104 B |  35.6 KB |   0.1% | ▏                |
| 19 | `int[]`                                           |  34.8 KB |  34.8 KB |   0.1% | ▏                |
| 20 | `int[]`                                           |  34.3 KB |  34.3 KB |   0.1% | ▏                |

### Biggest Classes by Retained Heap

_Classes whose instances together retain the most heap._

|  # | Class                                             | Instances | Retained Heap |                  |
| -: | ------------------------------------------------- | --------: | ------------: | ---------------- |
|  1 | `cafesat.sat.Vector`                              |    36,614 |        4.2 MB | ████████████████ |
|  2 | `scala.collection.immutable.$colon$colon`         |   146,148 |        3.4 MB | ████████████▊    |
|  3 | `java.net.URLClassLoader`                         |         2 |        2.7 MB | ██████████▍      |
|  4 | `scala.collection.immutable.Set$Set2`             |    44,626 |        2.7 MB | ██████████▍      |
|  5 | `int[]`                                           |    88,089 |        2.5 MB | █████████▌       |
|  6 | `cafesat.sat.Literal`                             |    62,129 |        2.4 MB | █████████        |
|  7 | `cafesat.sat.Solver$Clause`                       |    65,563 |        2.0 MB | ███████▋         |
|  8 | `byte[]`                                          |    16,736 |        1.3 MB | ████▉            |
|  9 | `java.lang.Object[]`                              |     8,721 |        1.1 MB | ████▎            |
| 10 | `scala.collection.immutable.BitmapIndexedSetNode` |    12,953 |        1.0 MB | ███▉             |
| 11 | `cafesat.asts.core.Trees$ConnectiveSymbol`        |    35,232 |      825.8 KB | ███              |
| 12 | `cafesat.asts.core.Trees$ConnectiveApplication`   |    35,002 |      820.4 KB | ███              |
| 13 | `java.lang.String`                                |    17,180 |      671.6 KB | ██▌              |
| 14 | `scala.collection.immutable.Set$Set3`             |     8,746 |      546.6 KB | ██               |
| 15 | `cafesat.asts.core.Trees$PredicateApplication`    |    18,304 |      429.0 KB | █▌               |
| 16 | `java.util.LinkedHashMap`                         |     5,826 |      364.1 KB | █▎               |
| 17 | `java.util.HashMap$Node`                          |     7,643 |      348.0 KB | █▎               |
| 18 | `cafesat.api.Formulas$Formula`                    |     8,907 |      347.9 KB | █▎               |
| 19 | `java.util.zip.ZipFile$Source`                    |        10 |      263.3 KB | ▉                |
| 20 | `java.lang.Class`                                 |     1,735 |      214.7 KB | ▊                |

### Top-Dominator Size Distribution

_Retained-size spread across all 644694 top-level dominators (the biggest memory contributors)._

- Dominators: 644,694
- Smallest / largest retained: 0 B / 2.6 MB
- Median retained: 24 B
- Total retained (top-level): 29.8 MB

`▂▂▂█▃▂▂▂▂▂▂▂▂▂▂▂▂▂`  (0 B – 2.6 MB)

|   Size ≤ |   Count |                  |
| -------: | ------: | ---------------- |
|      1 B |     478 | ▏                |
|      8 B |     248 | ▏                |
|     16 B |  11,224 | ▍                |
|     32 B | 413,264 | ████████████████ |
|     64 B | 170,866 | ██████▌          |
|    128 B |  44,307 | █▋               |
|    256 B |   2,049 | ▏                |
|    512 B |   1,731 | ▏                |
|   1.0 KB |     378 | ▏                |
|   2.0 KB |      59 | ▏                |
|   4.0 KB |      22 | ▏                |
|   8.0 KB |      22 | ▏                |
|  16.0 KB |      10 | ▏                |
|  32.0 KB |      15 | ▏                |
|  64.0 KB |       9 | ▏                |
| 128.0 KB |       6 | ▏                |
| 256.0 KB |       5 | ▏                |
|   4.0 MB |       1 | ▏                |

### Biggest Packages by Retained Heap

_Retained heap aggregated by package prefix (rows retaining <1% of the total are pruned); the tree shows nesting._

| Package            | Objects |  Shallow | Retained |                  |
| ------------------ | ------: | -------: | -------: | ---------------- |
| `cafesat`          | 263,266 |   7.6 MB |  11.2 MB | ████████████████ |
| ├─ `sat`           | 164,335 |   5.4 MB |   8.8 MB | ████████████▌    |
| ├─ `asts`          |  89,272 |   2.0 MB |   2.0 MB | ██▉              |
| │  └─ `core`       |  89,272 |   2.0 MB |   2.0 MB | ██▉              |
| └─ `api`           |   9,646 | 154.1 KB | 364.8 KB | ▌                |
| `scala`            | 212,882 |   5.1 MB |   7.8 MB | ███████████      |
| └─ `collection`    | 212,827 |   5.1 MB |   7.8 MB | ███████████      |
| │  └─ `immutable`  | 212,793 |   5.1 MB |   7.8 MB | ███████████      |
| `java`             |  61,857 |   2.1 MB |   6.7 MB | █████████▍       |
| ├─ `net`           |      97 |   6.6 KB |   2.7 MB | ███▉             |
| ├─ `lang`          |  35,067 |   1.1 MB |   2.2 MB | ███▏             |
| └─ `util`          |  25,794 | 984.2 KB |   1.6 MB | ██▎              |
| │  └─ `concurrent` |   3,432 | 143.5 KB | 389.0 KB | ▌                |
| `(primitives)`     | 104,866 |   3.9 MB |   3.9 MB | █████▌           |

## Dominator Analysis

### Big Drops

_Dominators where retained heap does not flow into a single child — the gap between an object's retained size and its largest child's retained size. A large drop means this object directly owns a lot of memory spread across many children (e.g. an array or collection). Threshold 0.3 MB (1% of reachable shallow). Multiple rows with the same class are distinct objects._

| Object                    |      # |    Retained | Largest Child         | Child Retained |       Drop |                  |
| ------------------------- | -----: | ----------: | --------------------- | -------------: | ---------: | ---------------- |
| `java.lang.Object[]`      |      1 |      2.5 MB | `java.lang.Object`    |           16 B |     2.5 MB | ████████████████ |
| `java.lang.Object[]`      |  50095 |      2.6 MB | `java.lang.Class`     |         2.5 MB |    67.5 KB | ▍                |
| `java.net.URLClassLoader` | 883965 |      2.6 MB | `java.util.ArrayList` |         2.6 MB |    43.9 KB | ▎                |
| `java.lang.Class`         | 876569 |      2.5 MB | `java.lang.Object[]`  |         2.5 MB |     1.1 KB | ▏                |
| `java.util.ArrayList`     |  50094 |      2.6 MB | `java.lang.Object[]`  |         2.6 MB |       24 B | ▏                |
| **Total**                 |        | **12.7 MB** |                       |    **10.1 MB** | **2.6 MB** |                  |

### Immediate Dominators

_Objects immediately dominated, rolled up by the dominator's class; a heavy dominated shallow heap under one class flags a retention hub._

| Dominator Class                                   | #Dominators |  #Dominated | Dominator Shallow | Dominated Shallow |                  |
| ------------------------------------------------- | ----------: | ----------: | ----------------: | ----------------: | ---------------- |
| `cafesat.sat.Vector`                              |      36,607 |      36,607 |          858.0 KB |            3.4 MB | ████████████████ |
| `java.lang.Object[]`                              |       5,833 |     161,270 |          845.2 KB |            3.0 MB | ██████████████   |
| `scala.collection.immutable.Set$Set2`             |      44,625 |      44,625 |            1.0 MB |            1.7 MB | ████████▏        |
| `java.lang.Class`                                 |       1,517 |       2,216 |           28.0 KB |          673.0 KB | ███▏             |
| `scala.collection.immutable.BitmapIndexedSetNode` |      14,921 |      15,070 |          582.9 KB |          404.6 KB | █▉               |
| `scala.collection.immutable.Set$Set3`             |       8,747 |       8,747 |          205.0 KB |          341.7 KB | █▌               |
| `java.lang.String`                                |       6,612 |       6,612 |          155.0 KB |          285.2 KB | █▎               |
| `java.util.zip.ZipFile$Source`                    |           2 |           2 |             160 B |          262.5 KB | █▏               |
| `cafesat.api.Formulas$Formula`                    |       8,906 |       8,906 |          139.2 KB |          208.7 KB | ▉                |
| `java.util.concurrent.ConcurrentHashMap$Node`     |       3,851 |       4,466 |          120.3 KB |          167.9 KB | ▊                |
| `java.util.HashMap$Node`                          |       5,085 |       5,117 |          158.9 KB |          126.9 KB | ▌                |
| `java.util.concurrent.ConcurrentHashMap$Node[]`   |          39 |       3,226 |           51.0 KB |          102.8 KB | ▍                |
| `byte[][]`                                        |           1 |         346 |            1.4 KB |           92.8 KB | ▍                |
| `java.net.URLClassLoader`                         |           2 |       2,568 |             176 B |           87.5 KB | ▍                |
| `cafesat.sat.Solver`                              |           1 |           1 |             168 B |           71.5 KB | ▎                |
| `java.util.HashMap$Node[]`                        |         136 |       1,856 |           49.9 KB |           58.1 KB | ▎                |
| `java.lang.Thread`                                |           5 |          20 |             520 B |           35.3 KB | ▏                |
| `java.util.HashMap`                               |          25 |          27 |            1.2 KB |           30.7 KB | ▏                |
| `java.lang.Object[][]`                            |          11 |         197 |            1.2 KB |           27.6 KB | ▏                |
| `jdk.internal.math.FDBigInteger`                  |         341 |         341 |           10.7 KB |           24.2 KB | ▏                |
| `java.util.concurrent.ConcurrentHashMap`          |          40 |          40 |            2.5 KB |           19.7 KB | ▏                |
| `scala.collection.immutable.$colon$colon`         |         805 |         805 |           18.9 KB |           18.9 KB | ▏                |
| `java.lang.Module`                                |          53 |         302 |            2.5 KB |           15.2 KB | ▏                |
| `java.lang.String[]`                              |           4 |         458 |            4.8 KB |           10.7 KB | ▏                |
| `java.lang.invoke.LambdaForm$Name`                |         385 |         391 |           12.0 KB |            9.4 KB | ▏                |
| `java.lang.Long[]`                                |           1 |         243 |            1.0 KB |            5.7 KB | ▏                |
| `scala.collection.immutable.BitmapIndexedMapNode` |         161 |         161 |            6.3 KB |            5.1 KB | ▏                |
| `char[][]`                                        |         103 |         207 |            2.4 KB |            4.8 KB | ▏                |
| `java.util.ArrayList`                             |           6 |           6 |             144 B |            4.8 KB | ▏                |
| `java.lang.invoke.DirectMethodHandle`             |         120 |         120 |            4.7 KB |            4.7 KB | ▏                |
| **Total**                                         | **138,945** | **304,953** |        **4.2 MB** |       **11.0 MB** |                  |

## Threads

### Thread Overview

_One row per resolved thread; columns mirror Eclipse MAT's Thread Overview._

| Name                           | Shallow | Retained | Max. Locals' Retained | Context Class Loader                   | Daemon | Priority | State                                                  |                  |
| ------------------------------ | ------: | -------: | --------------------: | -------------------------------------- | ------ | -------: | ------------------------------------------------------ | ---------------- |
| [main](#thread-1)              |   104 B |  35.6 KB |               71.7 KB | `java/net/URLClassLoader @ 0xc0412288` | no     |        5 | [alive, runnable]                                      | ████████████████ |
| [Reference Handler](#thread-2) |   104 B |    104 B |                   0 B | `—`                                    | yes    |       10 | [alive, runnable]                                      | ▏                |
| [Finalizer](#thread-3)         |   112 B |    168 B |                  40 B | `—`                                    | yes    |        8 | [alive, waiting, waiting indefinitely, in Object.wait] | ▏                |
| [Common-Cleaner](#thread-6)    |   112 B |    112 B |                 128 B | `—`                                    | yes    |        8 | [alive, waiting, waiting with timeout, parked]         | ▏                |

<a id="thread-1"></a>

### Thread 1 "main" (java/lang/Thread)

_Local roots: 124._

_Showing top 20 by retained heap (sizes overlap and do not sum to thread total)._

**Local root objects:**

| Object                                          | Count | Shallow | Retained |
| ----------------------------------------------- | ----: | ------: | -------: |
| `cafesat/sat/Solver`                            |    ×2 |   168 B |  71.7 KB |
| `scala/collection/immutable/SetIterator`        |    ×2 |    40 B |    112 B |
| `scala/collection/immutable/$colon$colon`       |    ×2 |    24 B |     24 B |
| `cafesat/sat/Solver$$Lambda+0x00007de4a41f7bd0` |     1 |    24 B |     24 B |
| `cafesat/asts/core/Trees$ConnectiveApplication` |     1 |    24 B |     24 B |
| `scala/collection/immutable/HashSet`            |    ×3 |    16 B |     16 B |
| `cafesat/sat/Solver$$Lambda+0x00007de4a41f5a08` |    ×3 |    16 B |     16 B |
| `scala/collection/immutable/Nil$`               |     1 |    16 B |     16 B |
| `cafesat/api/Solver$`                           |     1 |    16 B |     16 B |
| `[Lcafesat/sat/Literal;`                        |     1 |    16 B |     16 B |
| `scala/runtime/ObjectRef`                       |    ×2 |    16 B |     16 B |
| `cafesat/api/Formulas$Formula`                  |     1 |    16 B |     16 B |

_Frame percentages are of this thread's 35.6 KB retained heap._

- `scala.collection.IterableOnceOps.count (IterableOnce.scala:618)`
  - `scala.collection.immutable.SetIterator` retains 112 B (<0.1% of thread retained)
- `scala.collection.IterableOnceOps.exists (IterableOnce.scala:604)`
  - `scala.collection.immutable.SetIterator` retains 112 B (<0.1% of thread retained)
  - `cafesat.sat.Solver$$Lambda+0x00007de4a41f5a08` retains 16 B (<0.1% of thread retained)
- `scala.collection.IterableOnceOps.exists$ (IterableOnce.scala:601)`
  - `cafesat.sat.Solver$$Lambda+0x00007de4a41f5a08` retains 16 B (<0.1% of thread retained)
  - `scala.collection.immutable.HashSet` retains 16 B (<0.1% of thread retained)
- `scala.collection.AbstractIterable.exists (Iterable.scala:933)`
  - `cafesat.sat.Solver$$Lambda+0x00007de4a41f5a08` retains 16 B (<0.1% of thread retained)
  - `scala.collection.immutable.HashSet` retains 16 B (<0.1% of thread retained)
- `cafesat.sat.Solver.$anonfun$initClauses$1 (Solver.scala:124)`
  - `scala.collection.immutable.HashSet` retains 16 B (<0.1% of thread retained)
  - `scala.runtime.ObjectRef` retains 16 B (<0.1% of thread retained)
- `scala.collection.immutable.List.foreach (List.scala:333)`
  - `cafesat.sat.Solver$$Lambda+0x00007de4a41f7bd0` retains 24 B (<0.1% of thread retained)
  - `scala.collection.immutable.$colon$colon` retains 24 B (<0.1% of thread retained)
- `cafesat.sat.Solver.initClauses (Solver.scala:115)`
  - `cafesat.sat.Solver` retains 71.7 KB (0.2% of thread retained)
  - `scala.collection.immutable.$colon$colon` retains 24 B (<0.1% of thread retained)
  - `scala.runtime.ObjectRef` retains 16 B (<0.1% of thread retained)
- `cafesat.sat.Solver.solve (Solver.scala:147)`
  - `cafesat.sat.Solver` retains 71.7 KB (0.2% of thread retained)
  - `cafesat.sat.Literal[]` retains 16 B (<0.1% of thread retained)
  - `scala.collection.immutable.Nil$` retains 16 B (<0.1% of thread retained)
- `cafesat.api.Solver$.solveForSatisfiability (Solver.scala:84)`
  - `cafesat.asts.core.Trees$ConnectiveApplication` retains 24 B (<0.1% of thread retained)
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
| `java/lang/ref/NativeReferenceQueue`      |    ×3 |    40 B |     40 B |
| `java/lang/ref/NativeReferenceQueue$Lock` |    ×3 |    16 B |     16 B |
| `java/lang/System$2`                      |     1 |    16 B |     16 B |

_Frame percentages are of this thread's 168 B retained heap._

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
| `java/lang/Class`                                                       |    ×2 |    32 B |    128 B |
| `java/util/concurrent/TimeUnit`                                         |     1 |    80 B |     80 B |
| `java/util/concurrent/locks/AbstractQueuedSynchronizer$ConditionNode`   |     1 |    32 B |     32 B |
| `java/lang/ref/ReferenceQueue`                                          |    ×3 |    32 B |     32 B |
| `java/util/concurrent/locks/AbstractQueuedSynchronizer$ConditionObject` |    ×2 |    24 B |     24 B |
| `jdk/internal/ref/CleanerImpl`                                          |    ×3 |    24 B |     24 B |

_Frame percentages are of this thread's 112 B retained heap._

- `java.util.concurrent.locks.LockSupport.parkNanos (LockSupport.java:269)`
  - `java.util.concurrent.locks.AbstractQueuedSynchronizer$ConditionObject` retains 24 B (<0.1% of thread retained)
- `java.util.concurrent.locks.AbstractQueuedSynchronizer$ConditionObject.await (AbstractQueuedSynchronizer.java:1886)`
  - `java.util.concurrent.TimeUnit` retains 80 B (<0.1% of thread retained)
  - `java.util.concurrent.locks.AbstractQueuedSynchronizer$ConditionNode` retains 32 B (<0.1% of thread retained)
  - `java.util.concurrent.locks.AbstractQueuedSynchronizer$ConditionObject` retains 24 B (<0.1% of thread retained)
- `java.lang.ref.ReferenceQueue.await (ReferenceQueue.java:71)`
  - `java.lang.ref.ReferenceQueue` retains 32 B (<0.1% of thread retained)
- `java.lang.ref.ReferenceQueue.remove0 (ReferenceQueue.java:143)`
  - `java.lang.ref.ReferenceQueue` retains 32 B (<0.1% of thread retained)
- `java.lang.ref.ReferenceQueue.remove (ReferenceQueue.java:218)`
  - `java.lang.ref.ReferenceQueue` retains 32 B (<0.1% of thread retained)
- `jdk.internal.ref.CleanerImpl.run (CleanerImpl.java:140)`
  - `jdk.internal.ref.CleanerImpl` retains 24 B (<0.1% of thread retained)
- `java.lang.Thread.runWith (Thread.java:1596)`
  - `java.lang.Class` retains 128 B (<0.1% of thread retained)
  - `jdk.internal.ref.CleanerImpl` retains 24 B (<0.1% of thread retained)
- `java.lang.Thread.run (Thread.java:1583)`
  - `java.lang.Class` retains 128 B (<0.1% of thread retained)
  - `jdk.internal.ref.CleanerImpl` retains 24 B (<0.1% of thread retained)

## Top Components

_Retained heap grouped by class loader (component); `% Heap` is the share of total reachable heap._

| Component                                              | Retained | % Heap | Top classes                                                                                                                                                                                                                     |                  |
| ------------------------------------------------------ | -------: | -----: | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------------- |
| `java/net/URLClassLoader`                              |  25.5 MB |  49.5% | `cafesat.sat.Literal` (4.8 MB), `cafesat.sat.Vector` (4.2 MB), `cafesat.sat.Solver$Clause[]` (3.4 MB), `scala.collection.immutable.$colon$colon` (3.4 MB), `scala.collection.immutable.Set$Set2` (2.7 MB)                       | ████████████████ |
| `<boot>`                                               |  23.4 MB |  45.5% | `java.lang.Object[]` (4.5 MB), `java.lang.Class` (2.8 MB), `java.net.URLClassLoader` (2.7 MB), `int[]` (2.6 MB), `java.util.ArrayList` (2.6 MB)                                                                                 | ██████████████▋  |
| `java/net/URLClassLoader`                              |   2.6 MB |   5.0% | `scala.runtime.LazyVals$` (2.5 MB), `scala.math.BigInt$` (8.1 KB), `scala.math.BigInt[]` (8.0 KB), `scopt.OptionDef` (4.1 KB), `org.renaissance.harness.ConfigParser$$anon$1` (1.6 KB)                                          | █▌               |
| `jdk/internal/loader/ClassLoaders$AppClassLoader`      |  30.0 KB |   0.1% | `org.renaissance.core.BenchmarkDescriptor` (28.2 KB), `org.renaissance.core.BenchmarkSuite` (496 B), `org.renaissance.core.Launcher` (192 B), `org.renaissance.core.ModuleLoader` (64 B), `org.renaissance.core.Logging` (56 B) | ▏                |
| `jdk/internal/loader/ClassLoaders$PlatformClassLoader` |     24 B |  <0.1% | `sun.util.resources.cldr.provider.CLDRLocaleDataMetaInfo` (24 B)                                                                                                                                                                | ▏                |

## Arrays by Size

_Array-length distribution bucketed by power-of-two element length; `Max length` is the inclusive upper bound of each bucket._

### Object arrays

| Max length |    Objects |    Shallow |                  |
| ---------: | ---------: | ---------: | ---------------- |
|        ≤ 1 |      1,235 |    28.9 KB | ▌                |
|        ≤ 2 |     12,712 |   297.9 KB | █████▎           |
|        ≤ 4 |      7,554 |   236.1 KB | ███▏             |
|        ≤ 8 |      2,755 |   115.7 KB | █▏               |
|       ≤ 16 |      1,755 |   112.2 KB | ▋                |
|       ≤ 32 |     38,515 |     3.6 MB | ████████████████ |
|       ≤ 64 |        119 |    23.3 KB | ▏                |
|      ≤ 128 |         80 |    27.8 KB | ▏                |
|      ≤ 256 |         30 |    25.9 KB | ▏                |
|      ≤ 512 |         10 |    15.4 KB | ▏                |
|    ≤ 1,024 |         18 |    60.4 KB | ▏                |
|    ≤ 2,048 |          6 |    40.8 KB | ▏                |
|    ≤ 4,096 |          6 |    65.3 KB | ▏                |
|    ≤ 8,192 |          1 |    31.0 KB | ▏                |
|   ≤ 16,384 |          1 |    34.8 KB | ▏                |
|   ≤ 32,768 |          1 |    71.5 KB | ▏                |
|   ≤ 65,536 |          1 |   143.0 KB | ▏                |
|  ≤ 131,072 |          1 |   512.0 KB | ▏                |
|  **Total** | **64,800** | **5.4 MB** |                  |

### Primitive arrays

| Max length |     Objects |    Shallow |                  |
| ---------: | ----------: | ---------: | ---------------- |
|        ≤ 1 |         814 |    19.1 KB | ▏                |
|        ≤ 2 |      67,597 |     1.5 MB | ████████████████ |
|        ≤ 4 |      18,501 |   569.0 KB | ████▍            |
|        ≤ 8 |       5,167 |   162.6 KB | █▏               |
|       ≤ 16 |       5,752 |   224.5 KB | █▎               |
|       ≤ 32 |       6,779 |   304.0 KB | █▌               |
|       ≤ 64 |       8,570 |   546.1 KB | ██               |
|      ≤ 128 |       1,138 |   112.3 KB | ▎                |
|      ≤ 256 |         195 |    46.5 KB | ▏                |
|      ≤ 512 |         270 |   104.2 KB | ▏                |
|    ≤ 1,024 |          77 |    64.3 KB | ▏                |
|    ≤ 2,048 |          16 |    62.2 KB | ▏                |
|    ≤ 4,096 |           3 |     9.6 KB | ▏                |
|    ≤ 8,192 |           8 |    71.4 KB | ▏                |
|   ≤ 16,384 |           5 |   130.3 KB | ▏                |
|   ≤ 32,768 |           5 |   253.0 KB | ▏                |
|   ≤ 65,536 |           3 |   157.9 KB | ▏                |
|  ≤ 131,072 |           1 |   512.0 KB | ▏                |
|  ≤ 262,144 |           2 |   510.2 KB | ▏                |
|  **Total** | **114,903** | **5.3 MB** |                  |

Zero-length arrays: 2,401

## Collections

_Collection and array occupancy: how full collections are, how big they get, and constant primitive arrays._

### Collections by Kind

| Kind      |     Count | Total Elements | Max Elements | Total Shallow |                  |
| --------- | --------: | -------------: | -----------: | ------------: | ---------------- |
| list      |       105 |          1,261 |          485 |        2.5 KB | █▊               |
| map       |     6,202 |         11,394 |        2,889 |      381.8 KB | ████████████████ |
| **Total** | **6,307** |     **12,655** |              |  **384.3 KB** |                  |

### Collection Fill Ratio

_609 tracked of 6,307 collections._

|      Fill % | Collections |     Shallow |       Wasted |                  |
| ----------: | ----------: | ----------: | -----------: | ---------------- |
|       0–10% |         252 |     11.6 KB |      28.8 KB | ████████████████ |
|      10–20% |         136 |      6.3 KB |      15.8 KB | ████████▋        |
|      20–30% |          50 |      2.3 KB |       5.5 KB | ███▏             |
|      30–40% |          44 |      2.4 KB |      50.7 KB | ██▊              |
|      40–50% |          45 |      2.4 KB |      41.3 KB | ██▊              |
|      50–60% |          16 |       800 B |       7.3 KB | █                |
|      60–70% |          14 |       776 B |       5.9 KB | ▉                |
|      70–80% |          12 |       496 B |      19.3 KB | ▊                |
|      80–90% |           2 |        48 B |       1.2 KB | ▏                |
|     90–100% |           0 |         0 B |          0 B |                  |
| 100% (full) |          38 |       912 B |          0 B | ██▍              |
|   **Total** |     **609** | **28.0 KB** | **175.8 KB** |                  |

### Collections by Size

_6,307 tracked; 5,806 empty._

|    Size ≤ | Collections | Total Shallow |                  |
| --------: | ----------: | ------------: | ---------------- |
|       ≤ 1 |         204 |        9.0 KB | ████████████████ |
|       ≤ 2 |          94 |        3.6 KB | ███████▎         |
|       ≤ 4 |         100 |        4.1 KB | ███████▊         |
|       ≤ 8 |          37 |        1.8 KB | ██▉              |
|      ≤ 16 |          31 |        1.5 KB | ██▍              |
|      ≤ 32 |           9 |         408 B | ▋                |
|      ≤ 64 |           8 |         392 B | ▋                |
|     ≤ 128 |           4 |         224 B | ▎                |
|     ≤ 256 |           5 |         304 B | ▍                |
|     ≤ 512 |           4 |         144 B | ▎                |
|   ≤ 1,024 |           3 |         144 B | ▏                |
|   ≤ 4,096 |           2 |          96 B | ▏                |
| **Total** |     **501** |   **21.8 KB** |                  |

### Array Fill Ratio

_64,800 tracked object arrays._

|      Fill % |     Arrays |    Shallow |     Wasted |                  |
| ----------: | ---------: | ---------: | ---------: | ---------------- |
|       0–10% |     37,926 |     3.5 MB |     5.9 MB | ████████████████ |
|      10–20% |        193 |    27.2 KB |    40.3 KB | ▏                |
|      20–30% |         94 |     8.9 KB |    10.9 KB | ▏                |
|      30–40% |        126 |    52.4 KB |    67.0 KB | ▏                |
|      40–50% |        245 |   104.4 KB |   105.4 KB | ▏                |
|      50–60% |         46 |    10.2 KB |     8.8 KB | ▏                |
|      60–70% |         40 |     5.1 KB |     3.2 KB | ▏                |
|      70–80% |         30 |     3.4 KB |     1.5 KB | ▏                |
|      80–90% |         15 |     6.3 KB |     1.7 KB | ▏                |
|     90–100% |         16 |    36.8 KB |     2.1 KB | ▏                |
| 100% (full) |     26,069 |     1.6 MB |        0 B | ██████████▉      |
|   **Total** | **64,800** | **5.4 MB** | **6.1 MB** |                  |

### Map Collision Ratio

_493 tracked of 6,202 maps (occupied slots ÷ size; lower is worse)._

|      Load % |    Maps |     Shallow |                  |
| ----------: | ------: | ----------: | ---------------- |
|       0–10% |     221 |     11.0 KB | ████████████████ |
|      10–20% |     121 |      6.0 KB | ████████▊        |
|      20–30% |      40 |      2.2 KB | ██▉              |
|      30–40% |      48 |      2.7 KB | ███▍             |
|      40–50% |      50 |      2.7 KB | ███▌             |
|      50–60% |       8 |       448 B | ▌                |
|      60–70% |       2 |       128 B | ▏                |
|      70–80% |       3 |       160 B | ▏                |
|      80–90% |       0 |         0 B |                  |
|     90–100% |       0 |         0 B |                  |
| 100% (full) |       0 |         0 B |                  |
|   **Total** | **493** | **25.3 KB** |                  |

### Constant Primitive Arrays

_Primitive arrays whose every element is identical — possible candidates for deduplication or replacement with a shared constant. Short arrays (length < 8 with few instances) are hidden as noise._

_(34 trivial groups hidden.)_

| Array class |  Length |       Value | Objects |  Shallow |                  |
| ----------- | ------: | ----------: | ------: | -------: | ---------------- |
| `int[]`     | 131,064 |           0 |       1 | 512.0 KB | ▌                |
| `int[]`     |  18,307 |           0 |       1 |  71.5 KB | ▌                |
| `boolean[]` |  18,307 |           0 |       1 |  17.9 KB | ▌                |
| `char[]`    |   8,192 |           0 |       1 |  16.0 KB | ▌                |
| `int[]`     |   1,024 | -1059448624 |       1 |   4.0 KB | ▌                |
| `int[]`     |     802 |           0 |       1 |   3.1 KB | ▌                |
| `int[]`     |     616 |           0 |       1 |   2.4 KB | ▌                |
| `long[]`    |      32 |           0 |       4 |   1.1 KB | ██               |
| `byte[]`    |     512 |           0 |       2 |   1.0 KB | █                |
| `byte[]`    |       2 |          49 |      31 |    744 B | ████████████████ |
| `int[]`     |      32 |           0 |       4 |    576 B | ██               |
| `byte[]`    |     256 |           0 |       2 |    544 B | █                |
| `short[]`   |      32 |           0 |       4 |    320 B | ██               |
| `int[]`     |       2 |           0 |      11 |    264 B | █████▋           |
| `byte[]`    |       8 |           0 |      10 |    240 B | █████▏           |
| `int[]`     |      10 |           0 |       4 |    224 B | ██               |
| `char[]`    |      26 |           0 |       2 |    144 B | █                |
| `byte[]`    |     128 |           0 |       1 |    144 B | ▌                |
| `byte[]`    |      63 |          48 |       1 |     80 B | ▌                |
| `int[]`     |      16 |  -807617080 |       1 |     80 B | ▌                |
| `int[]`     |      16 |  -807616616 |       1 |     80 B | ▌                |
| `int[]`     |      16 |  -807616152 |       1 |     80 B | ▌                |
| `int[]`     |      16 |  -807262528 |       1 |     80 B | ▌                |
| `int[]`     |      12 |           0 |       1 |     64 B | ▌                |
| `byte[]`    |      10 |          32 |       1 |     32 B | ▌                |
| `byte[]`    |      13 |          48 |       1 |     32 B | ▌                |
| `byte[]`    |      16 |           0 |       1 |     32 B | ▌                |
| `byte[]`    |       8 |          32 |       1 |     24 B | ▌                |
| `byte[]`    |       8 |          48 |       1 |     24 B | ▌                |

### Top Arrays (primitive)

_The largest primitive arrays by shallow size, individually and aggregated by array class._

| Array class |  Length |    Shallow | Owner (Class#field)                  |                  |
| ----------- | ------: | ---------: | ------------------------------------ | ---------------- |
| `int[]`     | 131,064 |   512.0 KB | —                                    | ████████████████ |
| `byte[]`    | 261,187 |   255.1 KB | `java.util.zip.ZipFile$Source#cen`   | ███████▉         |
| `byte[]`    | 261,187 |   255.1 KB | —                                    | ███████▉         |
| `int[]`     |  18,307 |    71.5 KB | `cafesat.common.FixedIntStack#stack` | ██▏              |
| `int[]`     |  18,307 |    71.5 KB | `cafesat.sat.Solver#levels`          | ██▏              |
| `int[]`     |  18,307 |    71.5 KB | `cafesat.sat.Solver#model`           | ██▏              |
| `byte[]`    |  60,231 |    58.8 KB | —                                    | █▊               |
| `byte[]`    |  55,240 |    54.0 KB | `java.util.zip.ZipFile$Source#cen`   | █▋               |
| `byte[]`    |  46,187 |    45.1 KB | —                                    | █▍               |
| `int[]`     |   8,908 |    34.8 KB | `cafesat.sat.Solver$Clause#lits`     | █                |
| **Total**   |         | **1.4 MB** |                                      |                  |

#### Top Array Classes (primitive)

| Array class |   Instances |    Shallow |                  |
| ----------- | ----------: | ---------: | ---------------- |
| `int[]`     |      90,907 |     3.2 MB | ████████████████ |
| `byte[]`    |      25,805 |     2.1 MB | ██████████▎      |
| `char[]`    |         224 |    55.8 KB | ▎                |
| `boolean[]` |          11 |    19.8 KB | ▏                |
| `long[]`    |          17 |    14.2 KB | ▏                |
| `double[]`  |           8 |      688 B | ▏                |
| `short[]`   |           7 |      368 B | ▏                |
| `float[]`   |           4 |      112 B | ▏                |
| **Total**   | **116,983** | **5.3 MB** |                  |

### Top Arrays (object)

_The largest object arrays by shallow size, individually and aggregated by array class._

| Array class                      |  Length |     Used/Length |      Shallow | Owner (Class#field)          |                  |
| -------------------------------- | ------: | --------------: | -----------: | ---------------------------- | ---------------- |
| `java.lang.Object[]`             | 131,072 | 131,072/131,072 |     512.0 KB | —                            | ████████████████ |
| `cafesat.sat.Vector[]`           |  36,614 |   36,614/36,614 |     143.0 KB | `cafesat.sat.Solver#watched` | ████▍            |
| `cafesat.sat.Solver$Clause[]`    |  18,307 |        0/18,307 |      71.5 KB | `cafesat.sat.Solver#reasons` | ██▏              |
| `cafesat.api.Formulas$Formula[]` |   8,907 |     8,907/8,907 |      34.8 KB | —                            | █                |
| `java.lang.Object[]`             |   7,937 |     7,706/7,937 |      31.0 KB | —                            | ▉                |
| `java.util.HashMap$Node[]`       |   4,096 |     2,048/4,096 |      16.0 KB | `java.util.HashMap#table`    | ▌                |
| `java.util.HashMap$Node[]`       |   4,096 |     2,048/4,096 |      16.0 KB | `java.util.HashMap#table`    | ▌                |
| `java.lang.Object[]`             |   2,285 |       413/2,285 |       8.9 KB | —                            | ▎                |
| `java.lang.Object[]`             |   2,126 |     1,063/2,126 |       8.3 KB | —                            | ▎                |
| `scala.math.BigInt[]`            |   2,049 |         0/2,049 |       8.0 KB | —                            | ▎                |
| **Total**                        |         |                 | **849.7 KB** |                              |                  |

#### Top Array Classes (object)

| Array class                                     |  Instances |    Shallow |                  |
| ----------------------------------------------- | ---------: | ---------: | ---------------- |
| `cafesat.sat.Solver$Clause[]`                   |     36,615 |     3.4 MB | ████████████████ |
| `java.lang.Object[]`                            |     25,556 |     1.5 MB | ██████▉          |
| `cafesat.sat.Vector[]`                          |          1 |   143.0 KB | ▋                |
| `java.util.HashMap$Node[]`                      |        396 |    96.6 KB | ▍                |
| `java.util.concurrent.ConcurrentHashMap$Node[]` |         93 |    61.9 KB | ▎                |
| `cafesat.api.Formulas$Formula[]`                |          2 |    35.1 KB | ▏                |
| `java.lang.ref.SoftReference[]`                 |        434 |    32.2 KB | ▏                |
| `java.lang.Class[]`                             |        729 |    22.3 KB | ▏                |
| `java.lang.String[]`                            |        207 |    13.2 KB | ▏                |
| `java.lang.invoke.MethodHandle[]`               |         61 |    11.4 KB | ▏                |
| **Total**                                       | **64,094** | **5.3 MB** |                  |

## References

_Soft/weak/phantom reference referents (what they point at)._

### Soft References

_Soft references keep objects alive until the JVM needs memory — they are cleared under GC pressure. A large soft-referenced heap is often a cache that grows unbounded; consider bounding the cache size._

_281 reference instances._

#### Referent classes

| Class                                    | Objects | Shallow | Retained |                  |
| ---------------------------------------- | ------: | ------: | -------: | ---------------- |
| `java.lang.invoke.LambdaForm`            |     178 |  8.3 KB |   8.9 KB | ████████████████ |
| `java.lang.invoke.DirectMethodHandle`    |      34 |  1.3 KB |   1.5 KB | ██▋              |
| `java.lang.Class$ReflectionData`         |      21 |  1.3 KB |   1.3 KB | ██▎              |
| `sun.util.locale.BaseLocale`             |      20 |   640 B |    640 B | █▏               |
| `java.util.Locale`                       |      10 |   320 B |    608 B | █                |
| `java.util.jar.Manifest`                 |       8 |   192 B |    192 B | ▎                |
| `java.util.concurrent.ConcurrentHashMap` |       4 |   256 B |   1.6 KB | ██▉              |
| `[Ljava.lang.Object;`                    |       2 |    64 B |      0 B |                  |
| `java.util.ArrayList`                    |       1 |    24 B |     24 B | ▏                |
| `sun.text.resources.cldr.FormatData`     |       1 |    40 B |     40 B | ▏                |
| `sun.text.resources.cldr.FormatData_en`  |       1 |    40 B |     40 B | ▏                |
| `sun.util.resources.Bundles$1`           |       1 |    40 B |     40 B | ▏                |

#### Only-weakly retained _(approximate)_

_Objects with no incoming strong reference other than this reference chain — GC pressure would free them._

| Class                            | Objects | Shallow | Retained |                  |
| -------------------------------- | ------: | ------: | -------: | ---------------- |
| `java.lang.Class$ReflectionData` |      21 |  1.3 KB |   1.3 KB | ████████████████ |

### Weak References

_Weak references do not prevent GC. Objects listed here are reachable only via weak chains — under any GC they may be reclaimed. Large counts are usually benign._

_975 reference instances._

#### Referent classes

| Class                                                             | Objects | Shallow | Retained |                  |
| ----------------------------------------------------------------- | ------: | ------: | -------: | ---------------- |
| `java.lang.invoke.MethodType`                                     |     894 | 34.9 KB |  38.1 KB | ████████████████ |
| `java.lang.ClassValue$Identity`                                   |      11 |   176 B |    176 B | ▏                |
| `java.util.logging.Level`                                         |       9 |   288 B |    288 B | ▏                |
| `java.util.logging.Logger`                                        |       8 |   448 B |    448 B | ▏                |
| `java.lang.ClassValue$Version`                                    |       6 |   144 B |    144 B | ▏                |
| `java.lang.Module`                                                |       4 |   192 B |  19.6 KB | ████████▏        |
| `java.util.logging.LogManager$RootLogger`                         |       4 |   256 B |    256 B | ▏                |
| `sun.security.provider.FileInputStreamPool$UnclosableInputStream` |       2 |    32 B |     32 B | ▏                |
| `java.lang.ClassLoader`                                           |       1 |    16 B |      0 B |                  |
| `java.lang.ThreadGroup`                                           |       1 |    48 B |     48 B | ▏                |
| `java.net.URLClassLoader`                                         |       1 |     8 B |      0 B |                  |
| `java.security.Provider$Service`                                  |       1 |     8 B |      0 B |                  |
| `java.security.SecureClassLoader`                                 |       1 |     0 B |      0 B |                  |
| `jdk.internal.loader.BuiltinClassLoader`                          |       1 |    16 B |      0 B |                  |
| `jdk.internal.loader.ClassLoaders$AppClassLoader`                 |       1 |     8 B |      0 B |                  |
| `jdk.internal.loader.ClassLoaders$PlatformClassLoader`            |       1 |     8 B |      0 B |                  |
| `jdk.internal.misc.TerminatingThreadLocal$1`                      |       1 |    16 B |     16 B | ▏                |
| `org.renaissance.core.Launcher`                                   |       1 |    24 B |      0 B |                  |
| `scala.reflect.ClassTag$GenericClassTag`                          |       1 |    16 B |     16 B | ▏                |
| `scala.reflect.ManifestFactory$ObjectManifest`                    |       1 |    32 B |     32 B | ▏                |
_… 2 more classes (2 objects, 32 B shallow, 32 B retained)._

#### Only-weakly retained _(approximate)_

_Objects with no incoming strong reference other than this reference chain — GC pressure would free them._

_None found — no objects are exclusively reachable via this reference kind._

### Phantom References

_Phantom references mark objects in finalization or cleanup pipelines. A large backlog may indicate that the ReferenceQueue processor is too slow or blocked, or that native resources (file handles, native buffers) are not being released promptly._

_38 reference instances._

#### Referent classes

| Class                                 | Objects | Shallow | Retained |                  |
| ------------------------------------- | ------: | ------: | -------: | ---------------- |
| `java.io.FileDescriptor`              |      12 |   480 B |    480 B | ███████▊         |
| `java.util.zip.Inflater`              |      11 |   704 B |    704 B | ███████████▍     |
| `java.util.jar.JarFile`               |      10 |   640 B |    984 B | ████████████████ |
| `java.lang.ref.Cleaner`               |       1 |    16 B |     16 B | ▎                |
| `java.nio.DirectByteBuffer`           |       1 |    72 B |     72 B | █▏               |
| `sun.net.www.protocol.jar.URLJarFile` |       1 |    80 B |    208 B | ███▍             |
| `sun.nio.fs.NativeBuffer`             |       1 |    32 B |     32 B | ▌                |

#### Only-weakly retained _(approximate)_

_Objects with no incoming strong reference other than this reference chain — GC pressure would free them._

_None found — no objects are exclusively reachable via this reference kind._

## Unreachable Objects

_4,266 unreachable objects, 673.0 KB shallow heap (within the unreachable forest retained = shallow since all paths stay in-forest; top 30 classes by shallow)._

_Unreachable objects are eligible for collection but have not yet been reclaimed. A small unreachable heap (< 5% of heap total) is normal between GC cycles._

| Kind             | Objects |  Shallow |                  |
| ---------------- | ------: | -------: | ---------------- |
| Instances        |   1,364 |  38.6 KB | ▉                |
| Object arrays    |      60 |   3.7 KB | ▏                |
| Primitive arrays |   2,726 | 630.7 KB | ████████████████ |
| Class objects    |     116 |      0 B |                  |

_Shallow heap is additive; Retained sets overlap (nested subtrees are counted once per ancestor)._

| Class                                                                   | Objects |  Shallow | Retained |                  |
| ----------------------------------------------------------------------- | ------: | -------: | -------: | ---------------- |
| `int[]`                                                                 |   1,642 | 569.6 KB | 569.6 KB | ████████████████ |
| `byte[]`                                                                |   1,084 |  61.1 KB |  61.1 KB | █▋               |
| `java.lang.String`                                                      |   1,084 |  25.4 KB |  86.5 KB | ▋                |
| `java.lang.reflect.Field`                                               |      46 |   3.2 KB |   5.8 KB | ▏                |
| `java.lang.ClassValue$Entry[]`                                          |      12 |   1.7 KB |   1.7 KB | ▏                |
| `java.lang.reflect.Method`                                              |      18 |   1.5 KB |   3.2 KB | ▏                |
| `java.lang.Class$ReflectionData`                                        |      21 |   1.3 KB |  13.1 KB | ▏                |
| `java.util.WeakHashMap$Entry[]`                                         |      12 |    960 B |   1.4 KB | ▏                |
| `java.lang.invoke.MemberName`                                           |      21 |    840 B |    904 B | ▏                |
| `java.lang.ref.SoftReference`                                           |      21 |    840 B |  14.0 KB | ▏                |
| `java.lang.ClassValue$ClassValueMap`                                    |      12 |    768 B |   5.4 KB | ▏                |
| `java.lang.reflect.Constructor`                                         |       9 |    648 B |   2.0 KB | ▏                |
| `java.util.WeakHashMap$Entry`                                           |      11 |    440 B |    440 B | ▏                |
| `java.lang.ref.ReferenceQueue`                                          |      12 |    384 B |   1.2 KB | ▏                |
| `java.util.concurrent.locks.ReentrantLock$NonfairSync`                  |      12 |    384 B |    384 B | ▏                |
| `java.lang.invoke.DirectMethodHandle$Constructor`                       |       8 |    384 B |   1.1 KB | ▏                |
| `java.util.concurrent.locks.AbstractQueuedSynchronizer$ConditionObject` |      12 |    288 B |    288 B | ▏                |
| `java.lang.reflect.Field[]`                                             |       4 |    256 B |   6.1 KB | ▏                |
| `java.lang.reflect.Method[]`                                            |      10 |    248 B |   3.4 KB | ▏                |
| `java.lang.reflect.Constructor[]`                                       |      10 |    240 B |   2.3 KB | ▏                |
| `java.util.HashMap$Node`                                                |       7 |    224 B |    224 B | ▏                |
| `java.lang.Class[]`                                                     |       9 |    216 B |    216 B | ▏                |
| `java.lang.Thread`                                                      |       2 |    208 B |    520 B | ▏                |
| `java.lang.invoke.ResolvedMethodName`                                   |      12 |    192 B |    192 B | ▏                |
| `java.util.concurrent.locks.ReentrantLock`                              |      12 |    192 B |    192 B | ▏                |
| `jdk.internal.reflect.DirectConstructorHandleAccessor`                  |       8 |    192 B |   1.4 KB | ▏                |
| `java.lang.ClassValue$Entry`                                            |       6 |    192 B |    352 B | ▏                |
| `java.lang.ref.WeakReference`                                           |       5 |    160 B |    160 B | ▏                |
| `java.lang.invoke.DirectMethodHandle`                                   |       4 |    160 B |    384 B | ▏                |
| `java.lang.invoke.BoundMethodHandle$Species_L`                          |       4 |    160 B |    544 B | ▏                |

### Garbage-Root Dominator Trees

_Top garbage-root subtrees by retained heap (unreachable objects with no reachable predecessor). Depth capped._

1. **int[]** — 512.0 KB (1 object in subtree) ████████████████

2. **int[]** — 4.0 KB (1 object in subtree) ▏               

3. **java.lang.ref.SoftReference** — 3.2 KB (70 objects in subtree) ▏               
   └─ java.lang.Class$ReflectionData — 3.2 KB ▏               
        └─ java.lang.reflect.Field[] — 3.1 KB ▏               
             ├─ java.lang.reflect.Field — 200 B ▏               
             │    └─ jdk.internal.reflect.MethodHandleObjectFieldAccessorImpl — 128 B ▏               
             │         └─ java.lang.invoke.DirectMethodHandle$StaticAccessor — 96 B ▏               
             ├─ java.lang.reflect.Field — 144 B ▏               
             │    └─ java.lang.String — 72 B ▏               
             │         └─ byte[] — 48 B ▏               
             ├─ java.lang.reflect.Field — 144 B ▏               
             │    └─ java.lang.String — 72 B ▏               
             │         └─ byte[] — 48 B ▏               
             ├─ java.lang.reflect.Field — 136 B ▏               
             │    └─ java.lang.String — 64 B ▏               
             │         └─ byte[] — 40 B ▏               
             ├─ java.lang.reflect.Field — 136 B ▏               
             │    └─ java.lang.String — 64 B ▏               
             │         └─ byte[] — 40 B ▏               
             ├─ java.lang.reflect.Field — 136 B ▏               
             │    └─ java.lang.String — 64 B ▏               
             │         └─ byte[] — 40 B ▏               
             ├─ java.lang.reflect.Field — 136 B ▏               
             │    └─ java.lang.String — 64 B ▏               
             │         └─ byte[] — 40 B ▏               
             └─ java.lang.reflect.Field — 136 B ▏               
                  └─ java.lang.String — 64 B ▏               
                       └─ byte[] — 40 B ▏               

4. **int[]** — 3.1 KB (1 object in subtree) ▏               

5. **int[]** — 2.4 KB (1 object in subtree) ▏               

6. **int[]** — 2.4 KB (1 object in subtree) ▏               

7. **java.lang.ref.SoftReference** — 1.6 KB (36 objects in subtree) ▏               
   └─ java.lang.Class$ReflectionData — 1.6 KB ▏               
        └─ java.lang.reflect.Field[] — 1.5 KB ▏               
             ├─ java.lang.reflect.Field — 136 B ▏               
             │    └─ java.lang.String — 64 B ▏               
             │         └─ byte[] — 40 B ▏               
             ├─ java.lang.reflect.Field — 128 B ▏               
             │    └─ java.lang.String — 56 B ▏               
             │         └─ byte[] — 32 B ▏               
             ├─ java.lang.reflect.Field — 128 B ▏               
             │    └─ java.lang.String — 56 B ▏               
             │         └─ byte[] — 32 B ▏               
             ├─ java.lang.reflect.Field — 128 B ▏               
             │    └─ java.lang.String — 56 B ▏               
             │         └─ byte[] — 32 B ▏               
             ├─ java.lang.reflect.Field — 128 B ▏               
             │    └─ java.lang.String — 56 B ▏               
             │         └─ byte[] — 32 B ▏               
             ├─ java.lang.reflect.Field — 128 B ▏               
             │    └─ java.lang.String — 56 B ▏               
             │         └─ byte[] — 32 B ▏               
             ├─ java.lang.reflect.Field — 128 B ▏               
             │    └─ java.lang.String — 56 B ▏               
             │         └─ byte[] — 32 B ▏               
             └─ java.lang.reflect.Field — 120 B ▏               
                  └─ java.lang.String — 48 B ▏               
                       └─ byte[] — 24 B ▏               

8. **int[]** — 1.6 KB (1 object in subtree) ▏               

9. **java.lang.ref.SoftReference** — 1.1 KB (26 objects in subtree) ▏               
   └─ java.lang.Class$ReflectionData — 1.1 KB ▏               
        └─ java.lang.reflect.Field[] — 1016 B ▏               
             ├─ java.lang.reflect.Field — 200 B ▏               
             │    ├─ java.lang.String — 80 B ▏               
             │    │    └─ byte[] — 56 B ▏               
             │    └─ java.lang.String — 48 B ▏               
             │         └─ byte[] — 24 B ▏               
             ├─ java.lang.reflect.Field — 176 B ▏               
             │    ├─ java.lang.String — 56 B ▏               
             │    │    └─ byte[] — 32 B ▏               
             │    └─ java.lang.String — 48 B ▏               
             │         └─ byte[] — 24 B ▏               
             ├─ java.lang.reflect.Field — 136 B ▏               
             │    └─ java.lang.String — 64 B ▏               
             │         └─ byte[] — 40 B ▏               
             ├─ java.lang.reflect.Field — 136 B ▏               
             │    └─ java.lang.String — 64 B ▏               
             │         └─ byte[] — 40 B ▏               
             ├─ java.lang.reflect.Field — 128 B ▏               
             │    └─ java.lang.String — 56 B ▏               
             │         └─ byte[] — 32 B ▏               
             ├─ java.lang.reflect.Field — 120 B ▏               
             │    └─ java.lang.String — 48 B ▏               
             │         └─ byte[] — 24 B ▏               
             └─ java.lang.reflect.Field — 72 B ▏               

10. **java.lang.ref.SoftReference** — 912 B (19 objects in subtree) ▏               
   └─ java.lang.Class$ReflectionData — 872 B ▏               
        ├─ java.lang.reflect.Method[] — 544 B ▏               
        │    ├─ java.lang.reflect.Method — 304 B ▏               
        │    │    ├─ java.lang.String — 120 B ▏               
        │    │    │    └─ byte[] — 96 B ▏               
        │    │    ├─ java.lang.String — 72 B ▏               
        │    │    │    └─ byte[] — 48 B ▏               
        │    │    └─ java.lang.Class[] — 24 B ▏               
        │    └─ java.lang.reflect.Method — 216 B ▏               
        │         └─ java.lang.String — 128 B ▏               
        │              └─ byte[] — 104 B ▏               
        └─ java.lang.reflect.Constructor[] — 264 B ▏               
             └─ java.lang.reflect.Constructor — 240 B ▏               
                  └─ jdk.internal.reflect.DirectConstructorHandleAccessor — 168 B ▏               
                       └─ java.lang.invoke.DirectMethodHandle$Constructor — 144 B ▏               

## Allocation Sites

_Objects grouped by the stack trace that allocated them — each site is a candidate to allocate less by pooling, caching, or deferring construction. Shallow heap is additive; retained heap is not shown because summing per-object retained values over-counts shared subgraphs (a subtree retained by multiple sites is counted once per allocator, not once total)._

_Allocation-site records are present but contain no per-frame data. To capture method-level allocation stacks, run with JFR (`-XX:StartFlightRecording`) or attach a profiler before taking the heap dump._

## Retention Concentration

_Share of the reachable heap retained by the few largest top-level dominators (a dominator's retained size is everything it keeps alive). Read it as a concentration curve: if **Top 1** is already high, one object is the leak and freeing it reclaims most of the heap; if the share only climbs as you widen to **Top 10** / **Top 100**, the leak is spread across many peers (e.g. a big cache or collection of similar objects) and no single free helps much._

| Scope             | Retained Share |                  |
| ----------------- | -------------: | ---------------- |
| Top 1 object      |           8.8% | █▍               |
| Top 10 objects    |          13.1% | ██               |
| Top 100 objects   |          17.0% | ██▋              |
| Objects each >=1% |              1 |                  |

## Dominator-Depth Distribution

_How far each live object sits below a GC root, counted in dominator hops. Most objects clustering at shallow depths means memory is held close to the roots; a long tail means deep, chained structures (often a sign of nested collections or linked leaks)._

_Half of all live objects sit within 1 hop of a GC root; the deepest chain is 16 hops._

| Depth | Objects | % Objects | Cumulative % |                  |
| ----: | ------: | --------: | -----------: | ---------------- |
|     1 | 644,694 |     67.7% |        67.7% | ████████████████ |
|     2 | 146,089 |     15.3% |        83.0% | ███▋             |
|     3 |  10,776 |      1.1% |        84.1% | ▎                |
|     4 |   5,611 |      0.6% |        84.7% | ▏                |
|     5 |   3,139 |      0.3% |        85.1% | ▏                |
|     6 | 133,559 |     14.0% |        99.1% | ███▎             |
|     7 |   6,483 |      0.7% |        99.8% | ▏                |
|     8 |   1,191 |      0.1% |        99.9% | ▏                |
|     9 |   1,080 |      0.1% |       100.0% | ▏                |
|    10 |      23 |     <0.1% |       100.0% | ▏                |
|    11 |      13 |     <0.1% |       100.0% | ▏                |
|    12 |       3 |     <0.1% |       100.0% | ▏                |
|    13 |       1 |     <0.1% |       100.0% | ▏                |
|    14 |       1 |     <0.1% |       100.0% | ▏                |
|    15 |       1 |     <0.1% |       100.0% | ▏                |
|    16 |       1 |     <0.1% |       100.0% | ▏                |

## Leak Indicators

_Scalar signals for common Java leak patterns; non-zero values are flagged in [Memory Triage](#memory-triage) above. This table provides the raw numbers behind those bullets._

| Indicator                         |    Value |
| --------------------------------- | -------: |
| Anonymous/generated classes       |      178 |
| `DirectByteBuffer` total capacity | 134.3 MB |

## Glossary

_Definitions for the terms used above._

- **Shallow size**: the memory an object occupies by itself, meaning its header
  plus its own fields (and, for an array, its elements). It does *not* include the
  objects it points to.
- **Retained heap (retained size)**: the total memory that would be freed if this
  object were garbage-collected, meaning its own shallow size plus everything
  reachable *only* through it. This is the number that answers "how much does
  freeing this actually reclaim?" and it is the basis for every percentage in this
  report. See [dominator (graph theory)](https://en.wikipedia.org/wiki/Dominator_(graph_theory)).
- **Reachable heap**: all objects the [garbage collector](https://en.wikipedia.org/wiki/Garbage_collection_(computer_science)) can still
  reach from a GC root. Anything unreachable is already collectible and is excluded
  from the totals here.
- **GC root**: an object the JVM keeps alive unconditionally, such as live thread
  stacks (local variables), static fields of loaded classes,
  [JNI](https://en.wikipedia.org/wiki/Java_Native_Interface) references, and
  similar. Every retained-size chain ends at a GC root.
- **Dominator**: object *A* dominates object *B* if every path from a GC root to
  *B* passes through *A*. In other words, if *A* were freed, *B* would become
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
- **Map collision ratio** (load factor): for hash maps, the fraction of backing-array
  slots occupied — `occupied_slots / total_slots`. A low load factor means many
  empty buckets (wasted memory); a very high load factor increases hash collision
  probability and lookup cost.
- **Only-weakly retained**: an object that has no incoming strong reference — it is
  reachable only through one or more `WeakReference` or `SoftReference` chains.
  Under GC pressure these objects are candidates for collection.
- **Compressed OOPs** (Compressed Ordinary Object Pointers): a JVM optimisation
  where object references are stored as 32-bit integers instead of 64-bit pointers,
  halving reference-field overhead on heaps <= ~32 GB. Visible in the Heap Summary
  as `Compressed OOPs: yes`.
- **Class#field**: the notation used throughout this report to identify a specific
  field — `HolderClass#fieldName`. For example `java.util.HashMap#table` names the
  `table` field of `HashMap`. This is the dominant incoming reference path for an
  object, not a guaranteed allocation site — it is a hint, not a precise origin.
# Heap Dump Analysis: `dump_1_mnemonics.hprof`


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
| Total reachable heap | 11.3 MB |
| Objects              | 334,875 |
| Classes              |   2,285 |
| Class loaders        |       5 |
| Threads              |       4 |
| GC roots             |   1,553 |

**Top suspects by retained heap**

|  # | Suspect                                     | Retained | % Heap |
| -: | ------------------------------------------- | -------: | -----: |
|  1 | `byte[]` (48,035 instances)                 |   3.0 MB |  26.9% |
|  2 | `java.net.URLClassLoader` (single object)   |   2.6 MB |  23.2% |
|  3 | `java.util.HashMap$Node` (40,465 instances) |   1.9 MB |  17.2% |

**Likely problem:** retention is spread across several roots; no single object dominates.

## Memory Triage

_Where the reachable heap is concentrated, at a glance._

- **Headline retainer:** `byte[]` (a class group) retains 3.0 MB (26.9% of reachable heap). See [Leak Suspects](#leak-suspects).
- **Concentration:** diffuse — retention is spread across multiple roots, so there is no single object to free. See [Leak Suspects](#leak-suspects).
- **Shape:** deep (retention flows through long dominator chains — often nested collections or linked structures) — 90% of objects within depth 6, max depth 72. See [Dominator-Depth Distribution](#dominator-depth-distribution).
- **One leak or many:** the single biggest object, `java.net.URLClassLoader`, retains 23.2% and the top 10 retain 38.3% of the heap; 4 object(s) each hold >=1%. See [Top Consumers](#top-consumers).
- **Off-heap (DirectByteBuffer):** 134.3 MB of native memory is held by live DirectByteBuffers — not counted in heap size but can dominate RSS. See [Leak Indicators](#leak-indicators).
- **Fixed per-object header overhead:** 334,875 objects × 12 B header = 3.8 MB (34.1% of heap) is consumed by JVM object headers alone — consider value types, primitive arrays, or fewer wrapper objects. See [Header Overhead](#header-overhead).
- **Empty-collection cemetery:** 2,943 of 3,891 tracked collections (75.6%) are empty (size == 0) — pre-allocated but never populated containers waste object-header overhead; consider lazy initialisation or null. See [Collections](#collections).
- **Collection waste not analyzed:** _Collection waste not analyzed — re-run with `--collections` to check for wasted capacity._

## Waste Summary

_Approximately **1.2 MB** looks reclaimable across the sources below. Figures are approximate and may overlap slightly._

| Source                                     | Reclaimable |
| ------------------------------------------ | ----------: |
| [Under-filled object arrays](#collections) |    736.7 KB |
| [Under-filled collections](#collections)   |    485.2 KB |

## System Overview

_Reachable-heap totals and the largest classes by retained heap._

### Heap Summary

| Property                                      | Value                             |
| --------------------------------------------- | --------------------------------- |
| HPROF format                                  | JAVA PROFILE 1.0.2                |
| File size                                     | 20.3 MB                           |
| Identifier size                               | 64-bit                            |
| Compressed OOPs                               | yes                               |
| Dump created                                  | 2026-07-08T12:44:20Z              |
| Total objects                                 | 334,875                           |
| Total reachable heap                          | 11.3 MB                           |
| Off-heap / on-heap                            | 134.3 MB off-heap (11.9× on-heap) |
| GC roots                                      | 1,553                             |
| Classes loaded                                | 2,285                             |
| Class loaders                                 | 5                                 |
| Unreachable objects (excluded)                | 3,716 (659.6 KB)                  |
| Heap fragmentation (unreachable / heap total) | 5.4%                              |
| Top-class retained concentration              | 36.6%                             |

- **Class loaders (labels):** java/net/URLClassLoader, jdk/internal/loader/ClassLoaders$PlatformClassLoader, jdk/internal/loader/ClassLoaders$AppClassLoader

### GC Roots by Type

| Root Type    | Count |                  |
| ------------ | ----: | ---------------- |
| Sticky Class | 1,438 | ████████████████ |
| JNI Global   |   108 | █▏               |
| Thread       |     7 | ▏                |

### Heap Composition

| Kind             | Objects | Shallow Heap |                  |
| ---------------- | ------: | -----------: | ---------------- |
| Instances        | 266,242 |       5.7 MB | ████████████████ |
| Object arrays    |   4,667 |       1.2 MB | ███▍             |
| Primitive arrays |  61,681 |       4.3 MB | ████████████     |
| Class objects    |   2,285 |      29.4 KB | ▏                |

### HPROF Record Census

_Raw HPROF record-type composition of the dump (pass-1 counts). Useful for diagnosing truncated or unusual dumps (e.g. zero stack frames means no allocation-site data; a mismatch between load-class and class-dump counts can indicate a partial write). Additive, not parity-compared._

| Record Type           |   Count |
| --------------------- | ------: |
| UTF8 strings          |  61,050 |
| Load class            |   2,386 |
| Unload class          |       0 |
| Stack frames          |     287 |
| Stack traces          |       8 |
| Heap dump segments    |      21 |
| Instance dumps        | 267,339 |
| Object-array dumps    |   4,707 |
| Primitive-array dumps |  64,158 |
| Class dumps           |   2,386 |

#### GC Root Records by Tag

| Root Tag     | Count |
| ------------ | ----: |
| Sticky Class | 1,442 |
| Java Frame   |   356 |
| JNI Global   |   108 |
| Thread       |     7 |

### Duplicate Strings (approximate)

_Duplicate-string analysis not run (pass `--find-duplicates`)._

### Duplicate Primitive Arrays (approximate)

_Duplicate primitive-array analysis not run (pass `--find-duplicates`)._

### Boxed Numbers

_Wrapper types whose instances occupy heap that could be replaced with primitives._

|  # | Class                 | Instances | Total Shallow | % of Heap | Avg Size |
| -: | --------------------- | --------: | ------------: | --------: | -------: |
|  1 | `java.lang.Long`      |       256 |        6.0 KB |      0.1% |     24 B |
|  2 | `java.lang.Integer`   |       277 |        4.3 KB |     <0.1% |     16 B |
|  3 | `java.lang.Byte`      |       256 |        4.0 KB |     <0.1% |     16 B |
|  4 | `java.lang.Short`     |       256 |        4.0 KB |     <0.1% |     16 B |
|  5 | `java.lang.Character` |       128 |        2.0 KB |     <0.1% |     16 B |
|  6 | `java.lang.Boolean`   |         2 |          32 B |      0.0% |     16 B |
|  7 | `java.lang.Double`    |         1 |          24 B |      0.0% |     24 B |
|  8 | `java.lang.Float`     |         1 |          16 B |      0.0% |     16 B |

### Object Header Overhead

_Classes where object headers consume a large share of shallow heap. The practical action is to reduce object *count*: merge small objects, use primitive arrays instead of boxed wrappers, or replace fine-grained instances with a flat array of fields. Value types (Project Valhalla) eliminate headers entirely but are not yet generally available._

|  # | Class                                                 | Instances | Hdr/obj | Total Headers | Hdr % | Avg Size |
| -: | ----------------------------------------------------- | --------: | ------: | ------------: | ----: | -------: |
|  1 | `java.lang.Object`                                    |   132,975 |    12 B |        1.5 MB | 75.0% |     16 B |
|  2 | `java.lang.String`                                    |    59,804 |    12 B |      700.8 KB | 50.0% |     24 B |
|  3 | `java.util.HashMap$Node`                              |    47,746 |    12 B |      559.5 KB | 37.5% |     32 B |
|  4 | `java.util.concurrent.ConcurrentHashMap$Node`         |     5,958 |    12 B |       69.8 KB | 37.5% |     32 B |
|  5 | `java.util.jar.Attributes`                            |     2,895 |    12 B |       33.9 KB | 75.0% |     16 B |
|  6 | `java.lang.Class`                                     |     2,294 |    12 B |       26.9 KB | 89.6% |     13 B |
|  7 | `java.util.LinkedHashMap$Entry`                       |     1,179 |    12 B |       13.8 KB | 30.0% |     40 B |
|  8 | `java.lang.invoke.MemberName`                         |       782 |    12 B |        9.2 KB | 30.0% |     40 B |
|  9 | `jdk.internal.util.WeakReferenceKey`                  |       698 |    12 B |        8.2 KB | 37.5% |     32 B |
| 10 | `java.lang.invoke.MethodType`                         |       681 |    12 B |        8.0 KB | 30.0% |     40 B |
| 11 | `java.lang.invoke.ResolvedMethodName`                 |       598 |    12 B |        7.0 KB | 75.0% |     16 B |
| 12 | `java.lang.Class[]`                                   |       545 |    12 B |        6.4 KB | 40.9% |     29 B |
| 13 | `java.util.ArrayList`                                 |       528 |    12 B |        6.2 KB | 50.0% |     24 B |
| 14 | `java.lang.String[]`                                  |       450 |    12 B |        5.3 KB | 30.7% |     39 B |
| 15 | `java.lang.invoke.LambdaForm$Name`                    |       404 |    12 B |        4.7 KB | 37.5% |     32 B |
| 16 | `java.lang.module.ModuleDescriptor$Exports`           |       367 |    12 B |        4.3 KB | 50.0% |     24 B |
| 17 | `jdk.internal.math.FDBigInteger`                      |       341 |    12 B |        4.0 KB | 37.5% |     32 B |
| 18 | `java.lang.Integer`                                   |       277 |    12 B |        3.2 KB | 75.0% |     16 B |
| 19 | `sun.security.util.KnownOIDs`                         |       264 |    12 B |        3.1 KB | 30.0% |     40 B |
| 20 | `java.lang.Long`                                      |       256 |    12 B |        3.0 KB | 50.0% |     24 B |
| 21 | `java.lang.Byte`                                      |       256 |    12 B |        3.0 KB | 75.0% |     16 B |
| 22 | `java.lang.Short`                                     |       256 |    12 B |        3.0 KB | 75.0% |     16 B |
| 23 | `java.util.HashSet`                                   |       245 |    12 B |        2.9 KB | 75.0% |     16 B |
| 24 | `java.util.ImmutableCollections$Set12`                |       245 |    12 B |        2.9 KB | 50.0% |     24 B |
| 25 | `jdk.internal.module.ServicesCatalog$ServiceProvider` |       190 |    12 B |        2.2 KB | 50.0% |     24 B |
| 26 | `java.lang.invoke.DirectMethodHandle`                 |       187 |    12 B |        2.2 KB | 30.0% |     40 B |
| 27 | `java.lang.invoke.MethodTypeForm`                     |       167 |    12 B |        2.0 KB | 37.5% |     32 B |
| 28 | `java.lang.invoke.LambdaForm$NamedFunction`           |       157 |    12 B |        1.8 KB | 50.0% |     24 B |
| 29 | `java.util.ImmutableCollections$SetN`                 |       149 |    12 B |        1.7 KB | 50.0% |     24 B |
| 30 | `java.lang.ref.SoftReference`                         |       148 |    12 B |        1.7 KB | 30.0% |     40 B |

### Class Histogram (by Retained Heap)

_Top 50 classes ranked by retained heap; the full list is in the JSON output._

|  # | Class                                                  | Instances | Shallow Heap |  Largest | Retained Heap | % Heap |
| -: | ------------------------------------------------------ | --------: | -----------: | -------: | ------------: | -----: |
|  1 | `byte[]`                                               |    60,520 |       4.1 MB | 255.1 KB |        4.1 MB |  36.6% |
|  2 | `java.lang.Object[]`                                   |     2,329 |     722.5 KB | 512.0 KB |        3.4 MB |  30.4% |
|  3 | `java.lang.Class`                                      |     2,294 |      30.0 KB |   1.1 KB |        3.3 MB |  29.0% |
|  4 | `java.util.ArrayList`                                  |       528 |      12.4 KB |     24 B |        3.2 MB |  28.3% |
|  5 | `java.util.HashMap$Node`                               |    47,746 |       1.5 MB |     32 B |        3.1 MB |  27.3% |
|  6 | `java.net.URLClassLoader`                              |         2 |        176 B |     88 B |        2.6 MB |  23.2% |
|  7 | `scala.runtime.LazyVals$`                              |         1 |         16 B |     16 B |        2.5 MB |  22.2% |
|  8 | `java.lang.Object`                                     |   132,975 |       2.0 MB |     16 B |        2.0 MB |  18.0% |
|  9 | `java.lang.String`                                     |    59,804 |       1.4 MB |     24 B |        1.9 MB |  17.0% |
| 10 | `java.util.HashMap$Node[]`                             |       417 |     389.2 KB | 256.0 KB |        1.5 MB |  13.3% |
| 11 | `java.util.HashMap`                                    |       410 |      19.2 KB |     48 B |        1.4 MB |  12.1% |
| 12 | `jdk.internal.loader.ClassLoaders$AppClassLoader`      |         1 |         96 B |     96 B |      662.1 KB |   5.7% |
| 13 | `java.lang.ref.SoftReference`                          |       148 |       5.8 KB |     40 B |      618.5 KB |   5.4% |
| 14 | `jdk.internal.loader.URLClassPath`                     |         3 |        120 B |     40 B |      606.1 KB |   5.3% |
| 15 | `jdk.internal.loader.URLClassPath$JarLoader`           |         7 |        336 B |     48 B |      605.1 KB |   5.3% |
| 16 | `java.util.jar.JarFile`                                |         7 |        448 B |     64 B |      602.9 KB |   5.2% |
| 17 | `java.util.zip.ZipFile$CleanableResource`              |         8 |        256 B |     32 B |      599.8 KB |   5.2% |
| 18 | `java.util.ArrayDeque`                                 |        11 |        264 B |     24 B |      599.7 KB |   5.2% |
| 19 | `java.io.FileCleanable`                                |         9 |        504 B |     56 B |      599.6 KB |   5.2% |
| 20 | `java.util.zip.Inflater`                               |         8 |        512 B |     64 B |      599.4 KB |   5.2% |
| 21 | `jdk.internal.ref.CleanerImpl$PhantomCleanableRef`     |        18 |        864 B |     48 B |      599.3 KB |   5.2% |
| 22 | `java.util.zip.Inflater$InflaterZStreamRef`            |         8 |        192 B |     24 B |      599.3 KB |   5.2% |
| 23 | `java.util.jar.Manifest`                               |         6 |        144 B |     24 B |      592.1 KB |   5.1% |
| 24 | `java.util.zip.ZipFile$Source`                         |         7 |        560 B |     80 B |      449.7 KB |   3.9% |
| 25 | `java.util.concurrent.ConcurrentHashMap$Node`          |     5,958 |     186.2 KB |     32 B |      394.7 KB |   3.4% |
| 26 | `java.util.concurrent.ConcurrentHashMap$Node[]`        |        89 |      53.6 KB |   8.0 KB |      361.0 KB |   3.1% |
| 27 | `java.util.concurrent.ConcurrentHashMap`               |       113 |       7.1 KB |     64 B |      249.6 KB |   2.2% |
| 28 | `java.util.jar.Attributes`                             |     2,895 |      45.2 KB |     16 B |      240.2 KB |   2.1% |
| 29 | `java.util.LinkedHashMap`                              |     2,935 |     183.4 KB |     64 B |      197.4 KB |   1.7% |
| 30 | `int[]`                                                |       897 |     106.7 KB |  34.3 KB |      106.7 KB |   0.9% |
| 31 | `byte[][]`                                             |         1 |       1.4 KB |   1.4 KB |       94.1 KB |   0.8% |
| 32 | `jdk.internal.loader.ClassLoaders$PlatformClassLoader` |         1 |         96 B |     96 B |       70.5 KB |   0.6% |
| 33 | `java.lang.Module`                                     |        70 |       3.3 KB |     48 B |       65.5 KB |   0.6% |
| 34 | `jdk.internal.module.ModuleReferenceImpl`              |        62 |       3.4 KB |     56 B |       64.1 KB |   0.6% |
| 35 | `java.util.LinkedHashMap$Entry`                        |     1,179 |      46.1 KB |     40 B |       56.6 KB |   0.5% |
| 36 | `char[]`                                               |       223 |      55.7 KB |  16.0 KB |       55.7 KB |   0.5% |
| 37 | `java.lang.invoke.MethodType`                          |       681 |      26.6 KB |     40 B |       55.0 KB |   0.5% |
| 38 | `java.lang.invoke.MemberName`                          |       782 |      30.5 KB |     40 B |       51.6 KB |   0.4% |
| 39 | `java.lang.ModuleLayer`                                |         2 |         80 B |     40 B |       50.6 KB |   0.4% |
| 40 | `java.lang.module.ModuleDescriptor`                    |        62 |       3.9 KB |     64 B |       40.6 KB |   0.4% |
| 41 | `java.util.HashSet`                                    |       245 |       3.8 KB |     16 B |       38.9 KB |   0.3% |
| 42 | `java.lang.ref.SoftReference[]`                        |       272 |      20.2 KB |    120 B |       35.2 KB |   0.3% |
| 43 | `jdk.internal.math.FDBigInteger`                       |       341 |      10.7 KB |     32 B |       35.0 KB |   0.3% |
| 44 | `jdk.internal.loader.BuiltinClassLoader`               |         0 |          0 B |      0 B |       34.2 KB |   0.3% |
| 45 | `java.util.ImmutableCollections$SetN`                  |       149 |       3.5 KB |     24 B |       30.9 KB |   0.3% |
| 46 | `java.lang.invoke.LambdaForm$Name`                     |       404 |      12.6 KB |     32 B |       29.9 KB |   0.3% |
| 47 | `java.lang.CharacterData00`                            |         1 |         16 B |     16 B |       29.8 KB |   0.3% |
| 48 | `java.lang.String[]`                                   |       450 |      17.2 KB |   2.4 KB |       27.9 KB |   0.2% |
| 49 | `java.lang.invoke.MethodTypeForm`                      |       167 |       5.2 KB |     32 B |       25.4 KB |   0.2% |
| 50 | `jdk.internal.module.ServicesCatalog`                  |         4 |         64 B |     16 B |       23.8 KB |   0.2% |
_… 2,336 more classes, 320.5 KB shallow / 843.9 KB retained (full list in JSON)._

### Class Loaders

_Classes grouped by the loader that defined them. The **Loader** column shows the loader's class (e.g. `java/net/URLClassLoader`), not an instance name — the hprof format does not record loader names. Multiple rows with the same loader class are distinct loader instances; many such instances each holding significant heap can signal a classloader leak. The **Address** column distinguishes them._

| Loader                                               | Address    | Classes | Instances | Shallow Heap | Retained Heap |
| ---------------------------------------------------- | ---------- | ------: | --------: | -----------: | ------------: |
| <boot>                                               | <boot>     |   1,703 |   334,432 |      11.2 MB |       37.2 MB |
| java/net/URLClassLoader                              | 0xce800048 |     575 |       330 |      18.0 KB |        2.6 MB |
| jdk/internal/loader/ClassLoaders$AppClassLoader      | 0xffeecf48 |      90 |        68 |       1.4 KB |       21.8 KB |
| jdk/internal/loader/ClassLoaders$PlatformClassLoader | 0xffeec828 |       1 |         1 |         16 B |        7.4 KB |
| java/net/URLClassLoader                              | 0xc0412620 |      17 |        44 |        880 B |        2.6 KB |

## Leak Suspects

_Objects and class groups retaining the most heap, ranked by retained size. These are the most likely accumulation points for excessive memory usage. To fix: follow the dominator chain to the nearest object you control, and drop or null out the reference that keeps it alive. The path to GC root is shown for each suspect below — the tool cannot yet name the specific field; that requires field-labeled reference paths._

### 1. `byte[]` — retains 3.0 MB (26.9% of reachable heap)

48,035 instances of `byte[]` together retain this heap (combined shallow 3.0 MB).

#### Merged Paths to GC Roots

- `byte[]` (48,035 objects, retained 3.0 MB)
  - `byte[]` (48,035 objects, retained 3.0 MB)

### 2. `java.net.URLClassLoader` — retains 2.6 MB (23.2% of reachable heap)

One `java.net.URLClassLoader` object (shallow 88 B) dominates this retained heap.

Retained heap accumulates at `java.lang.Object[]` (retained 2.5 MB).

_Directly dominates 131,072 objects (showing top 1 classes by retained heap)._

**Accumulated objects by class:**

| Class              | Objects | Shallow | Retained | % of suspect |
| ------------------ | ------: | ------: | -------: | -----------: |
| `java.lang.Object` | 131,072 |  2.0 MB |   2.0 MB |        76.6% |

**Dominator chain to GC root:**

1. `java.net.URLClassLoader` (2.6 MB)

<details>
<summary>Dominator subtree</summary>

**Dominator subtree:**

- `java.lang.Object[]` (shallow 512.0 KB, retained 2.5 MB)
  - `java.lang.Object` ×4999 (shallow 16 B, retained 16 B each)

</details>

### 3. `java.util.HashMap$Node` — retains 1.9 MB (17.2% of reachable heap)

40,465 instances of `java.util.HashMap$Node` together retain this heap (combined shallow 1.2 MB).

#### Merged Paths to GC Roots

- `java.util.HashMap$Node` (40,465 objects, retained 1.9 MB)
  - `java.util.HashMap$Node` (40,465 objects, retained 1.9 MB)

## Top Consumers

### Biggest Objects (Top-Level Dominators)

_All top-level dominators ranked by retained heap. Unlike Leak Suspects, this list is unfiltered — it includes every object directly dominated by a GC root, down to the smallest. Use it when the suspect you care about didn't cross the leak-suspect threshold, or to see the full retention picture._

|  # | Class                                                  | Shallow | Retained | % Heap |
| -: | ------------------------------------------------------ | ------: | -------: | -----: |
|  1 | `java.net.URLClassLoader`                              |    88 B |   2.6 MB |  23.2% |
|  2 | `jdk.internal.loader.ClassLoaders$AppClassLoader`      |    96 B | 662.0 KB |   5.7% |
|  3 | `java.util.zip.ZipFile$Source`                         |    40 B | 448.0 KB |   3.9% |
|  4 | `java.util.HashMap`                                    |    48 B | 256.3 KB |   2.2% |
|  5 | `byte[][]`                                             |  1.4 KB |  94.1 KB |   0.8% |
|  6 | `jdk.internal.loader.ClassLoaders$PlatformClassLoader` |    96 B |  70.4 KB |   0.6% |
|  7 | `java.util.HashMap$Node[]`                             | 32.0 KB |  67.4 KB |   0.6% |
|  8 | `java.util.concurrent.ConcurrentHashMap$Node[]`        |  4.0 KB |  60.6 KB |   0.5% |
|  9 | `java.lang.ModuleLayer`                                |    40 B |  50.4 KB |   0.4% |
| 10 | `java.lang.Object[]`                                   |  8.9 KB |  35.8 KB |   0.3% |
| 11 | `jdk.internal.loader.BuiltinClassLoader`               |    16 B |  34.2 KB |   0.3% |
| 12 | `java.lang.Object[]`                                   | 31.0 KB |  31.0 KB |   0.3% |
| 13 | `java.lang.CharacterData00`                            |    40 B |  29.8 KB |   0.3% |
| 14 | `java.util.concurrent.ConcurrentHashMap$Node[]`        |  4.0 KB |  29.1 KB |   0.3% |
| 15 | `java.util.HashMap`                                    |    48 B |  27.5 KB |   0.2% |
| 16 | `java.lang.Class`                                      |    72 B |  19.5 KB |   0.2% |
| 17 | `java.io.PrintStream`                                  |    48 B |  17.3 KB |   0.1% |
| 18 | `java.util.concurrent.ConcurrentHashMap$Node[]`        |  4.0 KB |  17.0 KB |   0.1% |
| 19 | `java.util.HashMap$Node[]`                             |  4.0 KB |  16.8 KB |   0.1% |
| 20 | `char[]`                                               | 16.0 KB |  16.0 KB |   0.1% |

### Biggest Classes by Retained Heap

_Classes whose instances together retain the most heap._

|  # | Class                                                  | Instances | Retained Heap |
| -: | ------------------------------------------------------ | --------: | ------------: |
|  1 | `byte[]`                                               |    48,035 |        3.0 MB |
|  2 | `java.net.URLClassLoader`                              |         2 |        2.6 MB |
|  3 | `java.util.HashMap$Node`                               |    40,465 |        1.9 MB |
|  4 | `java.lang.String`                                     |    24,636 |      884.5 KB |
|  5 | `java.lang.Class`                                      |     1,617 |      745.7 KB |
|  6 | `jdk.internal.loader.ClassLoaders$AppClassLoader`      |         1 |      662.0 KB |
|  7 | `java.util.HashMap`                                    |       109 |      291.2 KB |
|  8 | `java.util.HashMap$Node[]`                             |       122 |      137.9 KB |
|  9 | `java.util.concurrent.ConcurrentHashMap$Node[]`        |        29 |      118.4 KB |
| 10 | `java.util.concurrent.ConcurrentHashMap$Node`          |     1,403 |      107.5 KB |
| 11 | `java.lang.Object[]`                                   |       496 |      101.5 KB |
| 12 | `byte[][]`                                             |         1 |       94.1 KB |
| 13 | `jdk.internal.loader.ClassLoaders$PlatformClassLoader` |         1 |       70.4 KB |
| 14 | `jdk.internal.module.ModuleReferenceImpl`              |        62 |       64.1 KB |
| 15 | `java.lang.ModuleLayer`                                |         1 |       50.4 KB |
| 16 | `java.util.LinkedHashMap$Entry`                        |     1,114 |       43.5 KB |
| 17 | `jdk.internal.math.FDBigInteger`                       |       340 |       34.8 KB |
| 18 | `java.lang.invoke.MethodType`                          |       296 |       32.3 KB |
| 19 | `java.util.ArrayList`                                  |       468 |       20.8 KB |
| 20 | `java.lang.String[]`                                   |       324 |       20.2 KB |

### Top-Dominator Size Distribution

_Retained-size spread across all 123989 top-level dominators (the biggest memory contributors)._

- Dominators: 123,989
- Smallest / largest retained: 0 B / 2.6 MB
- Median retained: 56 B
- Total retained (top-level): 11.3 MB

|   Size ≤ |  Count | % of Dom. |
| -------: | -----: | --------: |
|      1 B |    457 |      0.4% |
|      8 B |    175 |      0.1% |
|     16 B |  1,145 |      0.9% |
|     32 B | 34,093 |     27.5% |
|     64 B | 50,945 |     41.1% |
|    128 B | 36,269 |     29.3% |
|    256 B |    424 |      0.3% |
|    512 B |    256 |      0.2% |
|   1.0 KB |    121 |      0.1% |
|   2.0 KB |     41 |     <0.1% |
|   4.0 KB |     15 |     <0.1% |
|   8.0 KB |     16 |     <0.1% |
|  16.0 KB |     11 |     <0.1% |
|  32.0 KB |     10 |     <0.1% |
|  64.0 KB |      4 |     <0.1% |
| 128.0 KB |      3 |     <0.1% |
| 512.0 KB |      2 |     <0.1% |
|   1.0 MB |      1 |     <0.1% |
|   4.0 MB |      1 |     <0.1% |

### Biggest Packages by Retained Heap

_Retained heap aggregated by package prefix (rows retaining <1% of the total are pruned)._

| Package                | Objects |  Shallow | Retained |
| ---------------------- | ------: | -------: | -------: |
| `java`                 |  74,200 |   2.2 MB |   7.1 MB |
| `java.util`            |  44,927 |   1.5 MB |   3.2 MB |
| `java.util.zip`        |      24 |    504 B | 448.9 KB |
| `java.util.concurrent` |   1,564 |  67.2 KB | 245.0 KB |
| `java.net`             |      24 |   1.1 KB |   2.6 MB |
| `java.lang`            |  28,434 | 763.1 KB |   1.3 MB |
| `java.lang.invoke`     |   1,998 |  70.2 KB | 124.6 KB |
| `(primitives)`         |  48,151 |   3.1 MB |   3.2 MB |
| `jdk`                  |     689 |  20.2 KB | 903.2 KB |
| `jdk.internal`         |     686 |  20.2 KB | 903.2 KB |
| `jdk.internal.loader`  |      44 |    792 B | 773.6 KB |

## Dominator Analysis

### Big Drops

_Dominators where retained heap does not flow into a single child — the gap between an object's retained size and its largest child's retained size. A large drop means this object directly owns a lot of memory spread across many children (e.g. an array or collection). Threshold 0.1 MB (1% of reachable shallow). Multiple rows with the same class are distinct objects._

| Object                                             |      # |    Retained | Largest Child                             | Child Retained |       Drop |
| -------------------------------------------------- | -----: | ----------: | ----------------------------------------- | -------------: | ---------: |
| `java.lang.Object[]`                               |      1 |      2.5 MB | `java.lang.Object`                        |           16 B |     2.5 MB |
| `java.util.HashMap$Node[]`                         |  50504 |    577.2 KB | `java.util.HashMap$Node`                  |         1000 B |   576.2 KB |
| `java.util.HashMap$Node[]`                         | 176145 |    256.2 KB | `java.util.HashMap$Node`                  |           56 B |   256.2 KB |
| `byte[]`                                           | 194886 |    255.1 KB | —                                         |            0 B |   255.1 KB |
| `java.util.HashMap$Node[]`                         | 289922 |    447.8 KB | `java.util.HashMap$Node`                  |       295.8 KB |   152.0 KB |
| `java.lang.Object[]`                               |  42026 |      2.6 MB | `java.lang.Class`                         |         2.5 MB |    68.6 KB |
| `jdk.internal.loader.ClassLoaders$AppClassLoader`  | 336957 |    662.0 KB | `jdk.internal.loader.URLClassPath`        |       603.0 KB |    59.0 KB |
| `java.net.URLClassLoader`                          | 287272 |      2.6 MB | `java.util.ArrayList`                     |         2.6 MB |    44.2 KB |
| `java.util.zip.ZipFile$Source`                     | 314175 |    295.4 KB | `byte[]`                                  |       255.1 KB |    40.3 KB |
| `java.util.jar.Manifest`                           |  29414 |    584.7 KB | `java.util.HashMap`                       |       577.2 KB |     7.5 KB |
| `jdk.internal.ref.CleanerImpl$PhantomCleanableRef` | 314155 |    590.0 KB | `java.util.jar.JarFile`                   |       585.1 KB |     4.9 KB |
| `java.util.jar.JarFile`                            | 294408 |    602.2 KB | `java.util.zip.ZipFile$CleanableResource` |       599.8 KB |     2.4 KB |
| `jdk.internal.ref.CleanerImpl$PhantomCleanableRef` | 314143 |    598.0 KB | `java.util.zip.ZipFile$CleanableResource` |       595.7 KB |     2.3 KB |
| `jdk.internal.ref.CleanerImpl$PhantomCleanableRef` | 314146 |    595.1 KB | `java.util.zip.ZipFile$CleanableResource` |       593.4 KB |     1.7 KB |
| `jdk.internal.ref.CleanerImpl$PhantomCleanableRef` | 314149 |    592.9 KB | `java.util.zip.ZipFile$CleanableResource` |       591.4 KB |     1.5 KB |
| `java.lang.Class`                                  | 306832 |      2.5 MB | `java.lang.Object[]`                      |         2.5 MB |     1.2 KB |
| `jdk.internal.loader.URLClassPath$JarLoader`       | 294403 |    602.6 KB | `java.util.jar.JarFile`                   |       602.2 KB |      448 B |
| `jdk.internal.ref.CleanerImpl$PhantomCleanableRef` | 314141 |    598.9 KB | `java.util.zip.ZipFile$CleanableResource` |       598.5 KB |      392 B |
| `java.util.HashMap$Node`                           | 289923 |    295.8 KB | `java.util.zip.ZipFile$Source`            |       295.4 KB |      384 B |
| `java.util.jar.JarFile`                            |  29411 |    585.1 KB | `java.lang.ref.SoftReference`             |       584.8 KB |      352 B |
| `jdk.internal.ref.CleanerImpl$PhantomCleanableRef` | 314152 |    590.8 KB | `java.util.zip.ZipFile$CleanableResource` |       590.5 KB |      344 B |
| `java.util.zip.ZipFile$CleanableResource`          |     ×3 |    595.7 KB | `java.util.ArrayDeque`                    |       595.4 KB |      304 B |
| `jdk.internal.loader.URLClassPath`                 | 294396 |    603.0 KB | `java.util.ArrayList`                     |       602.7 KB |      304 B |
| **Total**                                          |        | **21.0 MB** |                                           |    **17.1 MB** | **3.9 MB** |

### Immediate Dominators

_Objects immediately dominated, rolled up by the dominator's class; a heavy dominated shallow heap under one class flags a retention hub._

| Dominator Class                                   | #Dominators |  #Dominated | Dominator Shallow | Dominated Shallow |
| ------------------------------------------------- | ----------: | ----------: | ----------------: | ----------------: |
| `java.lang.Object[]`                              |         208 |     133,824 |          545.7 KB |            2.1 MB |
| `java.util.HashMap$Node`                          |      33,973 |      38,059 |            1.0 MB |          888.7 KB |
| `java.lang.Class`                                 |       1,182 |       2,197 |           24.3 KB |          670.5 KB |
| `java.lang.String`                                |      11,503 |      11,503 |          269.6 KB |          556.2 KB |
| `java.util.zip.ZipFile$Source`                    |           7 |          26 |             560 B |          445.8 KB |
| `java.util.HashMap`                               |         223 |         226 |           10.5 KB |          304.0 KB |
| `java.util.jar.Attributes`                        |       2,895 |       2,895 |           45.2 KB |          180.9 KB |
| `java.util.concurrent.ConcurrentHashMap$Node`     |       3,660 |       4,751 |          114.4 KB |          170.9 KB |
| `java.util.HashMap$Node[]`                        |         266 |       5,189 |          348.3 KB |          162.1 KB |
| `java.util.concurrent.ConcurrentHashMap$Node[]`   |          57 |       4,323 |           48.6 KB |          138.3 KB |
| `byte[][]`                                        |           1 |         346 |            1.4 KB |           92.8 KB |
| `java.util.concurrent.ConcurrentHashMap`          |          61 |          67 |            3.8 KB |           34.5 KB |
| `jdk.internal.math.FDBigInteger`                  |         341 |         341 |           10.7 KB |           24.2 KB |
| `java.lang.Module`                                |          62 |         362 |            2.9 KB |           16.2 KB |
| `java.io.BufferedWriter`                          |           1 |           3 |              40 B |           16.1 KB |
| `java.util.ImmutableCollections$SetN`             |         147 |         147 |            3.4 KB |           14.5 KB |
| `java.util.ArrayList`                             |         212 |         212 |            5.0 KB |           13.7 KB |
| `jdk.internal.module.ModuleReferenceImpl`         |          62 |         248 |            3.4 KB |           11.1 KB |
| `java.lang.String[]`                              |           4 |         458 |            4.8 KB |           10.7 KB |
| `java.lang.invoke.MethodType`                     |         208 |         316 |            8.1 KB |            8.3 KB |
| `java.lang.invoke.LambdaForm$Name`                |         293 |         319 |            9.2 KB |            7.8 KB |
| `java.lang.invoke.MemberName`                     |         208 |         360 |            8.1 KB |            6.9 KB |
| `java.util.HashSet`                               |         139 |         139 |            2.2 KB |            6.5 KB |
| `java.lang.invoke.MethodTypeForm`                 |          38 |          81 |            1.2 KB |            5.8 KB |
| `java.lang.Long[]`                                |           1 |         243 |            1.0 KB |            5.7 KB |
| `java.lang.module.ModuleDescriptor`               |          62 |         237 |            3.9 KB |            5.6 KB |
| `java.lang.invoke.DirectMethodHandle$Constructor` |          77 |         125 |            3.6 KB |            4.9 KB |
| `java.lang.invoke.DirectMethodHandle`             |         117 |         124 |            4.6 KB |            4.9 KB |
| `char[][]`                                        |         103 |         207 |            2.4 KB |            4.8 KB |
| `java.util.LinkedHashMap$Entry`                   |          65 |         141 |            2.5 KB |            4.2 KB |
| **Total**                                         |  **56,176** | **207,469** |        **2.5 MB** |        **5.8 MB** |

## Threads

### Thread Overview

_One row per resolved thread; columns mirror Eclipse MAT's Thread Overview._

| Name                           | Shallow | Retained | Max. Locals' Retained | Context Class Loader                   | Daemon | Priority | State                                                  |
| ------------------------------ | ------: | -------: | --------------------: | -------------------------------------- | ------ | -------: | ------------------------------------------------------ |
| [main](#thread-1)              |   104 B |   2.2 KB |                 952 B | `java/net/URLClassLoader @ 0xc0412620` | no     |        5 | [alive, runnable]                                      |
| [Reference Handler](#thread-2) |   104 B |    104 B |                   0 B | `—`                                    | yes    |       10 | [alive, runnable]                                      |
| [Finalizer](#thread-3)         |   112 B |    128 B |                  40 B | `—`                                    | yes    |        8 | [alive, waiting, waiting indefinitely, in Object.wait] |
| [Common-Cleaner](#thread-6)    |   112 B |    192 B |                 128 B | `—`                                    | yes    |        8 | [alive, waiting, waiting with timeout, parked]         |

<a id="thread-1"></a>

### Thread 1 "main" (java/lang/Thread)

_Local roots: 329._

_Showing top 20 by retained heap (sizes overlap and do not sum to thread total)._

**Local root objects:**

| Object                                                   | Count | Shallow | Retained |
| -------------------------------------------------------- | ----: | ------: | -------: |
| `org/renaissance/jdk/streams/MnemonicsCoderWithStream`   |     1 |    24 B |    952 B |
| `[Ljava/lang/String;`                                    |     1 |   160 B |    160 B |
| `java/util/HashSet`                                      |     1 |    16 B |     64 B |
| `java/util/stream/ReduceOps$3ReducingSink`               |     1 |    32 B |     56 B |
| `java/util/stream/Collectors$CollectorImpl`              |     1 |    32 B |     48 B |
| `java/util/HashMap`                                      |     1 |    48 B |     48 B |
| `java/lang/String`                                       |     1 |    24 B |     48 B |
| `java/util/ArrayList$ArrayListSpliterator`               |     1 |    32 B |     32 B |
| `java/util/stream/ReduceOps$3ReducingSink`               |    ×3 |    32 B |     32 B |
| `java/lang/String`                                       |     1 |    24 B |     24 B |
| `java/util/stream/ReferencePipeline$7$1`                 |     1 |    24 B |     24 B |
| `java/util/stream/IntPipeline$1$1`                       |    ×2 |    24 B |     24 B |
| `java/lang/String`                                       |     1 |    24 B |     24 B |
| `[B`                                                     |     1 |    24 B |     24 B |
| `java/lang/String`                                       |     1 |    24 B |     24 B |
| `java/util/stream/Collectors$$Lambda+0x00007ff1d406e898` |     1 |    16 B |     16 B |
| `java/util/stream/Collectors$$Lambda+0x00007ff1d406eab8` |     1 |    16 B |     16 B |

_Frame percentages are of this thread's 2.2 KB retained heap._

- `java.lang.StringLatin1$CharsSpliterator.forEachRemaining (StringLatin1.java:811)`
  - `byte[]` retains 24 B (<0.1% of thread retained)
  - `java.util.stream.IntPipeline$1$1` retains 24 B (<0.1% of thread retained)
- `java.util.stream.AbstractPipeline.copyInto (AbstractPipeline.java:509)`
  - `java.util.stream.IntPipeline$1$1` retains 24 B (<0.1% of thread retained)
- `java.util.stream.AbstractPipeline.wrapAndCopyInto (AbstractPipeline.java:499)`
  - `java.util.stream.ReduceOps$3ReducingSink` retains 56 B (<0.1% of thread retained)
- `java.util.stream.Collectors.lambda$groupingBy$53 (Collectors.java:1105)`
  - `java.util.HashMap` retains 48 B (<0.1% of thread retained)
  - `java.lang.String` retains 24 B (<0.1% of thread retained)
  - `java.util.stream.Collectors$$Lambda+0x00007ff1d406e898` retains 16 B (<0.1% of thread retained)
  - `java.util.stream.Collectors$$Lambda+0x00007ff1d406eab8` retains 16 B (<0.1% of thread retained)
- `java.util.Spliterators$ArraySpliterator.forEachRemaining (Spliterators.java:1024)`
  - `java.lang.String[]` retains 160 B (<0.1% of thread retained)
  - `java.util.stream.ReduceOps$3ReducingSink` retains 32 B (<0.1% of thread retained)
- `java.util.stream.AbstractPipeline.copyInto (AbstractPipeline.java:509)`
  - `java.util.stream.ReduceOps$3ReducingSink` retains 32 B (<0.1% of thread retained)
- `java.util.stream.AbstractPipeline.wrapAndCopyInto (AbstractPipeline.java:499)`
  - `java.util.stream.ReduceOps$3ReducingSink` retains 32 B (<0.1% of thread retained)
- `java.util.stream.ReferencePipeline.collect (ReferencePipeline.java:682)`
  - `java.util.stream.Collectors$CollectorImpl` retains 48 B (<0.1% of thread retained)
- `org.renaissance.jdk.streams.MnemonicsCoderWithStream.encode (MnemonicsCoderWithStream.java:62)`
  - `org.renaissance.jdk.streams.MnemonicsCoderWithStream` retains 952 B (<0.1% of thread retained)
  - `java.util.HashSet` retains 64 B (<0.1% of thread retained)
  - `java.lang.String` retains 24 B (<0.1% of thread retained)
- `org.renaissance.jdk.streams.MnemonicsCoderWithStream.lambda$encode$9 (MnemonicsCoderWithStream.java:67)`
  - `java.lang.String` retains 48 B (<0.1% of thread retained)
  - `java.lang.String` retains 24 B (<0.1% of thread retained)
- `java.util.stream.ReferencePipeline$3$1.accept (ReferencePipeline.java:197)`
  - `java.util.stream.ReferencePipeline$7$1` retains 24 B (<0.1% of thread retained)
- `java.util.ArrayList$ArrayListSpliterator.forEachRemaining (ArrayList.java:1708)`
  - `java.util.ArrayList$ArrayListSpliterator` retains 32 B (<0.1% of thread retained)

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

_Frame percentages are of this thread's 128 B retained heap._

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
| `java/lang/ref/ReferenceQueue`                                          |    ×3 |    32 B |    112 B |
| `java/util/concurrent/TimeUnit`                                         |     1 |    80 B |     80 B |
| `java/util/concurrent/locks/AbstractQueuedSynchronizer$ConditionNode`   |     1 |    32 B |     32 B |
| `jdk/internal/ref/CleanerImpl`                                          |    ×3 |    24 B |     24 B |
| `java/util/concurrent/locks/AbstractQueuedSynchronizer$ConditionObject` |    ×2 |    24 B |     24 B |

_Frame percentages are of this thread's 192 B retained heap._

- `java.util.concurrent.locks.LockSupport.parkNanos (LockSupport.java:269)`
  - `java.util.concurrent.locks.AbstractQueuedSynchronizer$ConditionObject` retains 24 B (<0.1% of thread retained)
- `java.util.concurrent.locks.AbstractQueuedSynchronizer$ConditionObject.await (AbstractQueuedSynchronizer.java:1886)`
  - `java.util.concurrent.TimeUnit` retains 80 B (<0.1% of thread retained)
  - `java.util.concurrent.locks.AbstractQueuedSynchronizer$ConditionNode` retains 32 B (<0.1% of thread retained)
  - `java.util.concurrent.locks.AbstractQueuedSynchronizer$ConditionObject` retains 24 B (<0.1% of thread retained)
- `java.lang.ref.ReferenceQueue.await (ReferenceQueue.java:71)`
  - `java.lang.ref.ReferenceQueue` retains 112 B (<0.1% of thread retained)
- `java.lang.ref.ReferenceQueue.remove0 (ReferenceQueue.java:143)`
  - `java.lang.ref.ReferenceQueue` retains 112 B (<0.1% of thread retained)
- `java.lang.ref.ReferenceQueue.remove (ReferenceQueue.java:218)`
  - `java.lang.ref.ReferenceQueue` retains 112 B (<0.1% of thread retained)
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

| Component                                              | Retained | % Heap | Top classes                                                                                                                                                                                                                                                                                                                                                             |
| ------------------------------------------------------ | -------: | -----: | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `<boot>`                                               |  37.2 MB |  93.5% | `byte[]` (4.1 MB), `java.lang.Object[]` (3.4 MB), `java.lang.Class` (3.3 MB), `java.util.ArrayList` (3.2 MB), `java.util.HashMap$Node` (3.1 MB)                                                                                                                                                                                                                         |
| `java/net/URLClassLoader`                              |   2.6 MB |   6.4% | `scala.runtime.LazyVals$` (2.5 MB), `org.renaissance.harness.RenaissanceSuite$` (14.0 KB), `scala.math.BigInt$` (8.1 KB), `scala.math.BigInt[]` (8.0 KB), `scopt.OptionDef` (4.3 KB)                                                                                                                                                                                    |
| `jdk/internal/loader/ClassLoaders$AppClassLoader`      |  21.8 KB |   0.1% | `org.renaissance.BenchmarkResult$Validators` (3.9 KB), `org.renaissance.core.ModuleLoader` (3.5 KB), `org.renaissance.core.BenchmarkDescriptor` (3.2 KB), `org.renaissance.core.BenchmarkSuite` (2.5 KB), `org.renaissance.core.ResourceUtils` (2.3 KB)                                                                                                                 |
| `jdk/internal/loader/ClassLoaders$PlatformClassLoader` |   7.4 KB |  <0.1% | `sun.util.resources.cldr.provider.CLDRLocaleDataMetaInfo` (7.4 KB)                                                                                                                                                                                                                                                                                                      |
| `java/net/URLClassLoader`                              |   2.6 KB |  <0.1% | `org.renaissance.jdk.streams.MnemonicsCoderWithStream` (1.6 KB), `org.renaissance.jdk.streams.MnemonicsCoderWithStream$$Lambda+0x00007ff1d4127800` (384 B), `org.renaissance.jdk.streams.MnemonicsCoderWithStream$$Lambda+0x00007ff1d4126b18` (256 B), `org.renaissance.jdk.streams.Mnemonics` (200 B), `org.renaissance.jdk.streams.MnemonicsCoderWithStream$1` (72 B) |

## Arrays by Size

_Array-length distribution bucketed by power-of-two element length; `Max length` is the inclusive upper bound of each bucket._

### Object arrays

| Max length |   Objects |    Shallow |
| ---------: | --------: | ---------: |
|        ≤ 1 |       701 |    16.4 KB |
|        ≤ 2 |       553 |    13.0 KB |
|        ≤ 4 |       670 |    20.9 KB |
|        ≤ 8 |       651 |    27.1 KB |
|       ≤ 16 |     1,232 |    80.7 KB |
|       ≤ 32 |       334 |    38.2 KB |
|       ≤ 64 |       126 |    26.4 KB |
|      ≤ 128 |        64 |    23.2 KB |
|      ≤ 256 |        26 |    23.2 KB |
|      ≤ 512 |        10 |    15.4 KB |
|    ≤ 1,024 |        17 |    58.2 KB |
|    ≤ 2,048 |         5 |    32.8 KB |
|    ≤ 4,096 |         5 |    57.3 KB |
|    ≤ 8,192 |         2 |    63.0 KB |
|   ≤ 65,536 |         1 |   256.0 KB |
|  ≤ 131,072 |         1 |   512.0 KB |
|  **Total** | **4,398** | **1.2 MB** |

### Primitive arrays

| Max length |    Objects |    Shallow |
| ---------: | ---------: | ---------: |
|        ≤ 1 |        480 |    11.2 KB |
|        ≤ 2 |        687 |    16.1 KB |
|        ≤ 4 |      1,388 |    32.9 KB |
|        ≤ 8 |      2,411 |    58.0 KB |
|       ≤ 16 |      4,324 |   143.2 KB |
|       ≤ 32 |      5,713 |   255.5 KB |
|       ≤ 64 |     45,922 |     3.0 MB |
|      ≤ 128 |        858 |    85.7 KB |
|      ≤ 256 |        181 |    41.2 KB |
|      ≤ 512 |        268 |   101.3 KB |
|    ≤ 1,024 |         74 |    57.0 KB |
|    ≤ 2,048 |         15 |    50.4 KB |
|    ≤ 4,096 |          3 |     9.6 KB |
|    ≤ 8,192 |          8 |    83.9 KB |
|   ≤ 16,384 |          3 |    61.2 KB |
|   ≤ 65,536 |          2 |   104.0 KB |
|  ≤ 131,072 |          1 |   512.0 KB |
|  ≤ 262,144 |          1 |   255.1 KB |
|  **Total** | **62,339** | **4.9 MB** |

Zero-length arrays: 2,128

## Collections

_Collection and array occupancy: how full collections are, how big they get, and constant primitive arrays._

### Collections by Kind

| Kind      |     Count | Total Elements | Max Elements | Total Shallow |
| --------- | --------: | -------------: | -----------: | ------------: |
| list      |       531 |          1,362 |          463 |       12.4 KB |
| map       |     3,360 |         48,945 |       32,500 |      203.5 KB |
| **Total** | **3,891** |     **50,307** |              |  **215.9 KB** |

### Collection Fill Ratio

_1,050 tracked of 3,891 collections._

|      Fill % | Collections |     Shallow |       Wasted |
| ----------: | ----------: | ----------: | -----------: |
|       0–10% |         563 |     18.9 KB |      50.5 KB |
|      10–20% |         217 |      8.2 KB |      20.9 KB |
|      20–30% |          66 |      2.6 KB |      18.0 KB |
|      30–40% |          59 |      2.8 KB |      35.4 KB |
|      40–50% |          62 |      3.1 KB |     299.2 KB |
|      50–60% |          16 |       800 B |       7.3 KB |
|      60–70% |          17 |       920 B |      43.4 KB |
|      70–80% |          10 |       424 B |       9.8 KB |
|      80–90% |           2 |        48 B |        752 B |
|     90–100% |           0 |         0 B |          0 B |
| 100% (full) |          38 |       912 B |          0 B |
|   **Total** |   **1,050** | **38.7 KB** | **485.2 KB** |

### Collections by Size

_3,891 tracked; 2,943 empty._

|    Size ≤ | Collections | Total Shallow |
| --------: | ----------: | ------------: |
|       ≤ 1 |         520 |       16.5 KB |
|       ≤ 2 |         176 |        5.6 KB |
|       ≤ 4 |         131 |        4.8 KB |
|       ≤ 8 |          38 |        1.9 KB |
|      ≤ 16 |          30 |        1.5 KB |
|      ≤ 32 |          26 |        1.2 KB |
|      ≤ 64 |           8 |         392 B |
|     ≤ 128 |           4 |         224 B |
|     ≤ 256 |           5 |         304 B |
|     ≤ 512 |           3 |         120 B |
|   ≤ 1,024 |           3 |         144 B |
|   ≤ 4,096 |           2 |          96 B |
|   ≤ 8,192 |           1 |          48 B |
|  ≤ 32,768 |           1 |          48 B |
| **Total** |     **948** |   **32.8 KB** |

### Array Fill Ratio

_4,398 tracked object arrays._

|      Fill % |    Arrays |    Shallow |       Wasted |
| ----------: | --------: | ---------: | -----------: |
|       0–10% |     1,253 |    98.3 KB |     149.5 KB |
|      10–20% |       271 |    29.2 KB |      41.5 KB |
|      20–30% |       105 |    18.2 KB |      24.2 KB |
|      30–40% |       139 |   301.1 KB |     368.9 KB |
|      40–50% |       245 |   131.6 KB |     136.2 KB |
|      50–60% |        40 |     9.9 KB |       8.6 KB |
|      60–70% |        36 |     4.7 KB |       2.9 KB |
|      70–80% |        29 |     3.5 KB |       1.6 KB |
|      80–90% |        14 |     4.2 KB |       1.2 KB |
|     90–100% |        14 |    36.3 KB |       2.1 KB |
| 100% (full) |     2,252 |   627.1 KB |          0 B |
|   **Total** | **4,398** | **1.2 MB** | **736.7 KB** |

### Map Collision Ratio

_511 tracked of 3,360 maps (occupied slots ÷ size; lower is worse)._

|      Load % |    Maps |     Shallow |
| ----------: | ------: | ----------: |
|       0–10% |     221 |     11.0 KB |
|      10–20% |     123 |      6.1 KB |
|      20–30% |      38 |      2.1 KB |
|      30–40% |      66 |      3.5 KB |
|      40–50% |      50 |      2.7 KB |
|      50–60% |       8 |       448 B |
|      60–70% |       2 |       128 B |
|      70–80% |       3 |       160 B |
|      80–90% |       0 |         0 B |
|     90–100% |       0 |         0 B |
| 100% (full) |       0 |         0 B |
|   **Total** | **511** | **26.0 KB** |

### Constant Primitive Arrays

_Primitive arrays whose every element is identical — possible candidates for deduplication or replacement with a shared constant. Short arrays (length < 8 with few instances) are hidden as noise._

_(33 trivial groups hidden.)_

| Array class |  Length | Value | Objects |  Shallow |
| ----------- | ------: | ----: | ------: | -------: |
| `int[]`     | 131,064 |     0 |       1 | 512.0 KB |
| `char[]`    |   8,192 |     0 |       1 |  16.0 KB |
| `int[]`     |     834 |     0 |       1 |   3.3 KB |
| `int[]`     |     766 |     0 |       1 |   3.0 KB |
| `long[]`    |      32 |     0 |       4 |   1.1 KB |
| `byte[]`    |     512 |     0 |       2 |   1.0 KB |
| `byte[]`    |       2 |    49 |      31 |    744 B |
| `int[]`     |      32 |     0 |       4 |    576 B |
| `byte[]`    |     256 |     0 |       2 |    544 B |
| `short[]`   |      32 |     0 |       4 |    320 B |
| `int[]`     |      10 |     0 |       5 |    280 B |
| `int[]`     |       2 |     0 |      10 |    240 B |
| `byte[]`    |       8 |     0 |       7 |    168 B |
| `char[]`    |      26 |     0 |       2 |    144 B |
| `byte[]`    |     128 |     0 |       1 |    144 B |
| `byte[]`    |      63 |    48 |       1 |     80 B |
| `int[]`     |      16 |     0 |       1 |     80 B |
| `int[]`     |      12 |     0 |       1 |     64 B |
| `byte[]`    |      10 |    32 |       1 |     32 B |
| `byte[]`    |      13 |    48 |       1 |     32 B |
| `byte[]`    |      16 |     0 |       1 |     32 B |
| `byte[]`    |       8 |    32 |       1 |     24 B |
| `byte[]`    |       8 |    48 |       1 |     24 B |

### Top Arrays (primitive)

_The largest primitive arrays by shallow size, individually and aggregated by array class._

| Array class |  Length |      Shallow | Owner (Class#field)                    |
| ----------- | ------: | -----------: | -------------------------------------- |
| `int[]`     | 131,064 |     512.0 KB | —                                      |
| `byte[]`    | 261,187 |     255.1 KB | `java.util.zip.ZipFile$Source#cen`     |
| `byte[]`    |  60,231 |      58.8 KB | `java.util.zip.ZipFile$Source#cen`     |
| `byte[]`    |  46,187 |      45.1 KB | `java.util.zip.ZipFile$Source#cen`     |
| `int[]`     |   8,781 |      34.3 KB | `java.util.zip.ZipFile$Source#entries` |
| `int[]`     |   4,546 |      17.8 KB | —                                      |
| `char[]`    |   8,192 |      16.0 KB | `java.io.BufferedWriter#cb`            |
| `char[]`    |   8,192 |      16.0 KB | `java.io.BufferedWriter#cb`            |
| `byte[]`    |  15,354 |      15.0 KB | `java.util.zip.ZipFile$Source#cen`     |
| `byte[]`    |  12,096 |      11.8 KB | `java.lang.String#value`               |
| **Total**   |         | **982.0 KB** |                                        |

#### Top Array Classes (primitive)

| Array class |  Instances |    Shallow |
| ----------- | ---------: | ---------: |
| `byte[]`    |     61,405 |     4.2 MB |
| `int[]`     |      2,489 |   683.6 KB |
| `char[]`    |        223 |    55.7 KB |
| `long[]`    |         16 |    14.2 KB |
| `boolean[]` |          9 |     1.9 KB |
| `double[]`  |          7 |      672 B |
| `short[]`   |          6 |      352 B |
| `float[]`   |          3 |       96 B |
| **Total**   | **64,158** | **4.9 MB** |

### Top Arrays (object)

_The largest object arrays by shallow size, individually and aggregated by array class._

| Array class                                     |  Length |     Used/Length |      Shallow | Owner (Class#field)                            |
| ----------------------------------------------- | ------: | --------------: | -----------: | ---------------------------------------------- |
| `java.lang.Object[]`                            | 131,072 | 131,072/131,072 |     512.0 KB | —                                              |
| `java.util.HashMap$Node[]`                      |  65,536 |   25,551/65,536 |     256.0 KB | `java.util.HashMap#table`                      |
| `java.util.HashMap$Node[]`                      |   8,192 |     3,685/8,192 |      32.0 KB | `java.util.HashMap#table`                      |
| `java.lang.Object[]`                            |   7,937 |     7,706/7,937 |      31.0 KB | —                                              |
| `java.util.HashMap$Node[]`                      |   4,096 |     2,048/4,096 |      16.0 KB | `java.util.HashMap#table`                      |
| `java.util.HashMap$Node[]`                      |   4,096 |     1,889/4,096 |      16.0 KB | `java.util.HashMap#table`                      |
| `java.lang.Object[]`                            |   2,285 |       405/2,285 |       8.9 KB | —                                              |
| `java.lang.Object[]`                            |   2,126 |     1,063/2,126 |       8.3 KB | `java.util.ImmutableCollections$SetN#elements` |
| `scala.math.BigInt[]`                           |   2,049 |         0/2,049 |       8.0 KB | —                                              |
| `java.util.concurrent.ConcurrentHashMap$Node[]` |   2,048 |       544/2,048 |       8.0 KB | `java.util.concurrent.ConcurrentHashMap#table` |
| **Total**                                       |         |                 | **896.4 KB** |                                                |

#### Top Array Classes (object)

| Array class                                     | Instances |    Shallow |
| ----------------------------------------------- | --------: | ---------: |
| `java.lang.Object[]`                            |     2,310 |   722.2 KB |
| `java.util.HashMap$Node[]`                      |       418 |   389.3 KB |
| `java.util.concurrent.ConcurrentHashMap$Node[]` |        89 |    53.6 KB |
| `java.lang.ref.SoftReference[]`                 |       272 |    20.2 KB |
| `java.lang.Class[]`                             |       549 |    15.8 KB |
| `java.lang.String[]`                            |       208 |    13.4 KB |
| `scala.math.BigInt[]`                           |         1 |     8.0 KB |
| `java.lang.invoke.MethodHandle[]`               |        48 |     7.8 KB |
| `java.lang.invoke.LambdaForm$Name[]`            |       143 |     6.0 KB |
| `java.nio.ByteBuffer[]`                         |         1 |     4.0 KB |
| **Total**                                       | **4,039** | **1.2 MB** |

## References

_Soft/weak/phantom reference referents (what they point at)._

### Soft References

_Soft references keep objects alive until the JVM needs memory — they are cleared under GC pressure. A large soft-referenced heap is often a cache that grows unbounded; consider bounding the cache size._

_218 reference instances._

#### Referent classes

| Class                                    | Objects | Shallow | Retained |
| ---------------------------------------- | ------: | ------: | -------: |
| `java.lang.invoke.LambdaForm`            |     130 |  6.1 KB |  33.8 KB |
| `java.lang.Class$ReflectionData`         |      21 |  1.3 KB |   1.3 KB |
| `java.lang.invoke.DirectMethodHandle`    |      21 |   840 B |   3.4 KB |
| `sun.util.locale.BaseLocale`             |      20 |   640 B |    640 B |
| `java.util.Locale`                       |      10 |   320 B |    608 B |
| `java.util.jar.Manifest`                 |       6 |   144 B | 592.0 KB |
| `java.util.concurrent.ConcurrentHashMap` |       4 |   256 B |   2.3 KB |
| `[Ljava.lang.Object;`                    |       2 |    64 B |      0 B |
| `java.util.ArrayList`                    |       1 |    24 B |     24 B |
| `sun.text.resources.cldr.FormatData`     |       1 |    40 B |     40 B |
| `sun.text.resources.cldr.FormatData_en`  |       1 |    40 B |     40 B |
| `sun.util.resources.Bundles$1`           |       1 |    40 B |     40 B |

#### Only-weakly retained _(approximate)_

_Objects with no incoming strong reference other than this reference chain — GC pressure would free them._

| Class                            | Objects | Shallow | Retained |
| -------------------------------- | ------: | ------: | -------: |
| `java.lang.Class$ReflectionData` |      21 |  1.3 KB |   1.3 KB |

### Weak References

_Weak references do not prevent GC. Objects listed here are reachable only via weak chains — under any GC they may be reclaimed. Large counts are usually benign._

_745 reference instances._

#### Referent classes

| Class                                                             | Objects | Shallow | Retained |
| ----------------------------------------------------------------- | ------: | ------: | -------: |
| `java.lang.invoke.MethodType`                                     |     681 | 26.6 KB |  76.9 KB |
| `java.util.logging.Level`                                         |       9 |   288 B |    288 B |
| `java.util.logging.Logger`                                        |       8 |   448 B |   4.5 KB |
| `java.lang.Module`                                                |       4 |   192 B |  26.6 KB |
| `java.util.logging.LogManager$RootLogger`                         |       4 |   256 B |   1.4 KB |
| `java.lang.ClassValue$Version`                                    |       3 |    72 B |     72 B |
| `java.lang.ClassValue$Identity`                                   |       2 |    32 B |     32 B |
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
| `scala.reflect.ManifestFactory$ObjectManifest`                    |       1 |    32 B |     32 B |
| `sun.nio.ch.Util$1`                                               |       1 |    16 B |     16 B |
_… 1 more classes (1 objects, 16 B shallow, 16 B retained)._

#### Only-weakly retained _(approximate)_

_Objects with no incoming strong reference other than this reference chain — GC pressure would free them._

_None found — no objects are exclusively reachable via this reference kind._

### Phantom References

_Phantom references mark objects in finalization or cleanup pipelines. A large backlog may indicate that the ReferenceQueue processor is too slow or blocked, or that native resources (file handles, native buffers) are not being released promptly._

_29 reference instances._

#### Referent classes

| Class                                 | Objects | Shallow | Retained |
| ------------------------------------- | ------: | ------: | -------: |
| `java.io.FileDescriptor`              |       9 |   360 B |   1.0 KB |
| `java.util.zip.Inflater`              |       8 |   512 B |   3.5 MB |
| `java.util.jar.JarFile`               |       7 |   448 B |   1.2 MB |
| `java.lang.ref.Cleaner`               |       1 |    16 B |     16 B |
| `java.nio.DirectByteBuffer`           |       1 |    72 B |     72 B |
| `sun.net.www.protocol.jar.URLJarFile` |       1 |    80 B |    344 B |
| `sun.nio.fs.NativeBuffer`             |       1 |    32 B |    176 B |

#### Only-weakly retained _(approximate)_

_Objects with no incoming strong reference other than this reference chain — GC pressure would free them._

_None found — no objects are exclusively reachable via this reference kind._

## Unreachable Objects

_3,716 unreachable objects, 659.6 KB shallow heap (within the unreachable forest retained = shallow since all paths stay in-forest; top 30 classes by shallow)._

_Unreachable objects are eligible for collection but have not yet been reclaimed. At 5.4% of heap total (reachable + unreachable) this is elevated — the JVM may not have had time to GC before the dump was taken, or finalization may be backed up._

| Kind             | Objects |  Shallow |
| ---------------- | ------: | -------: |
| Instances        |   1,098 |  31.7 KB |
| Object arrays    |      40 |   1.6 KB |
| Primitive arrays |   2,477 | 626.4 KB |
| Class objects    |     101 |      0 B |

_Shallow heap is additive; Retained sets overlap (nested subtrees are counted once per ancestor)._

| Class                                                  | Objects |  Shallow | Retained |
| ------------------------------------------------------ | ------: | -------: | -------: |
| `int[]`                                                |   1,592 | 576.9 KB | 576.9 KB |
| `byte[]`                                               |     885 |  49.5 KB |  49.5 KB |
| `java.lang.String`                                     |     885 |  20.7 KB |  70.3 KB |
| `java.lang.reflect.Field`                              |      46 |   3.2 KB |   5.8 KB |
| `java.lang.reflect.Method`                             |      18 |   1.5 KB |   3.2 KB |
| `java.lang.Class$ReflectionData`                       |      21 |   1.3 KB |  13.1 KB |
| `java.lang.invoke.MemberName`                          |      21 |    840 B |    904 B |
| `java.lang.ref.SoftReference`                          |      21 |    840 B |  14.0 KB |
| `java.lang.reflect.Constructor`                        |       9 |    648 B |   2.0 KB |
| `java.lang.invoke.DirectMethodHandle$Constructor`      |       8 |    384 B |   1.1 KB |
| `java.lang.ClassValue$Entry[]`                         |       2 |    288 B |    288 B |
| `java.lang.reflect.Field[]`                            |       4 |    256 B |   6.1 KB |
| `java.lang.reflect.Method[]`                           |      10 |    248 B |   3.4 KB |
| `java.lang.reflect.Constructor[]`                      |      10 |    240 B |   2.3 KB |
| `java.util.HashMap$Node`                               |       7 |    224 B |    224 B |
| `java.lang.Class[]`                                    |       9 |    216 B |    216 B |
| `java.lang.Thread`                                     |       2 |    208 B |    520 B |
| `java.lang.invoke.ResolvedMethodName`                  |      12 |    192 B |    192 B |
| `jdk.internal.reflect.DirectConstructorHandleAccessor` |       8 |    192 B |   1.4 KB |
| `java.lang.invoke.DirectMethodHandle`                  |       4 |    160 B |    384 B |
| `java.lang.invoke.BoundMethodHandle$Species_L`         |       4 |    160 B |    544 B |
| `java.util.WeakHashMap$Entry[]`                        |       2 |    160 B |    240 B |
| `jdk.internal.reflect.DirectMethodHandleAccessor`      |       4 |    128 B |    672 B |
| `java.lang.ClassValue$ClassValueMap`                   |       2 |    128 B |    992 B |
| `java.util.WeakHashMap$Entry`                          |       2 |     80 B |     80 B |
| `java.security.AccessControlContext`                   |       2 |     80 B |     80 B |
| `java.lang.Thread$FieldHolder`                         |       2 |     80 B |     80 B |
| `java.lang.invoke.BoundMethodHandle$Species_LL`        |       2 |     80 B |    224 B |
| `java.util.HashMap$Node[]`                             |       1 |     80 B |    304 B |
| `java.lang.ref.ReferenceQueue`                         |       2 |     64 B |    208 B |

### Garbage-Root Dominator Trees

_Top garbage-root subtrees by retained heap (unreachable objects with no reachable predecessor). Depth capped._

1. **int[]** — 512.0 KB (1 object in subtree)

2. **int[]** — 17.8 KB (1 object in subtree)

3. **int[]** — 3.3 KB (1 object in subtree)

4. **java.lang.ref.SoftReference** — 3.2 KB (70 objects in subtree)
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

5. **int[]** — 3.0 KB (1 object in subtree)

6. **int[]** — 2.1 KB (1 object in subtree)

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

8. **java.lang.ref.SoftReference** — 1.1 KB (26 objects in subtree)
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

9. **java.lang.ref.SoftReference** — 912 B (19 objects in subtree)
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

10. **int[]** — 816 B (1 object in subtree)

## Allocation Sites

_Objects grouped by the stack trace that allocated them — each site is a candidate to allocate less by pooling, caching, or deferring construction. Shallow heap is additive; retained heap is not shown because summing per-object retained values over-counts shared subgraphs (a subtree retained by multiple sites is counted once per allocator, not once total)._

_Allocation-site records are present but contain no per-frame data. To capture method-level allocation stacks, run with JFR (`-XX:StartFlightRecording`) or attach a profiler before taking the heap dump._

## Retention Concentration

_Share of the reachable heap retained by the few largest top-level dominators (a dominator's retained size is everything it keeps alive). Read it as a concentration curve: if **Top 1** is already high, one object is the leak and freeing it reclaims most of the heap; if the share only climbs as you widen to **Top 10** / **Top 100**, the leak is spread across many peers (e.g. a big cache or collection of similar objects) and no single free helps much._

| Scope           | Retained Share | Retained |
| --------------- | -------------: | -------: |
| Top 1 object    |          23.2% |   2.6 MB |
| Top 10 objects  |          38.3% |   4.3 MB |
| Top 100 objects |          43.2% |   4.9 MB |

_4 objects each hold ≥1% of the reachable heap._

## Dominator-Depth Distribution

_How far each live object sits below a GC root, counted in dominator hops. Most objects clustering at shallow depths means memory is held close to the roots; a long tail means deep, chained structures (often a sign of nested collections or linked leaks)._

_Half of all live objects sit within 2 hops of a GC root; the deepest chain is 72 hops._

| Depth | Objects | % Objects | Cumulative % |
| ----: | ------: | --------: | -----------: |
|     1 | 123,989 |     37.0% |        37.0% |
|     2 |  46,824 |     14.0% |        51.0% |
|     3 |   4,334 |      1.3% |        52.3% |
|     4 |   5,439 |      1.6% |        53.9% |
|     5 |   4,055 |      1.2% |        55.1% |
|     6 | 132,759 |     39.6% |        94.8% |
|     7 |     683 |      0.2% |        95.0% |
|     8 |     398 |      0.1% |        95.1% |
|     9 |     200 |      0.1% |        95.2% |
|    10 |     104 |     <0.1% |        95.2% |
|    11 |     120 |     <0.1% |        95.2% |
|    12 |      91 |     <0.1% |        95.3% |
|    13 |      70 |     <0.1% |        95.3% |
|    14 |      66 |     <0.1% |        95.3% |
|    15 |      66 |     <0.1% |        95.3% |
|    16 |     104 |     <0.1% |        95.3% |
|    17 |      86 |     <0.1% |        95.4% |
|    18 |      46 |     <0.1% |        95.4% |
|    19 |      46 |     <0.1% |        95.4% |
|    20 |      40 |     <0.1% |        95.4% |
|    21 |      35 |     <0.1% |        95.4% |
|    22 |      32 |     <0.1% |        95.4% |
|    23 |      22 |     <0.1% |        95.4% |
|    24 |      23 |     <0.1% |        95.4% |
|    25 |      26 |     <0.1% |        95.5% |
|    26 |      41 |     <0.1% |        95.5% |
|    27 |      33 |     <0.1% |        95.5% |
|    28 |      28 |     <0.1% |        95.5% |
|    29 |      23 |     <0.1% |        95.5% |
|    30 |      31 |     <0.1% |        95.5% |
|    31 |      28 |     <0.1% |        95.5% |
|    32 |      36 |     <0.1% |        95.5% |
|    33 |      36 |     <0.1% |        95.5% |
|    34 |      34 |     <0.1% |        95.5% |
|    35 |      40 |     <0.1% |        95.6% |
|    36 |      35 |     <0.1% |        95.6% |
|    37 |      25 |     <0.1% |        95.6% |
|    38 |      29 |     <0.1% |        95.6% |
|    39 |      30 |     <0.1% |        95.6% |
|    40 |      31 |     <0.1% |        95.6% |
|    41 |      19 |     <0.1% |        95.6% |
|    42 |      13 |     <0.1% |        95.6% |
|    43 |      10 |     <0.1% |        95.6% |
|    44 |      14 |     <0.1% |        95.6% |
|    45 |      12 |     <0.1% |        95.6% |
|    46 |      12 |     <0.1% |        95.6% |
|    47 |      11 |     <0.1% |        95.6% |
|    48 |      11 |     <0.1% |        95.6% |
|    49 |       6 |     <0.1% |        95.6% |
|    50 |       6 |     <0.1% |        95.6% |

_… (+22 deeper buckets, 14,622 objects, 100.0% cumulative — full data in JSON)_

## Leak Indicators

_Scalar signals for common Java leak patterns; non-zero values are flagged in [Memory Triage](#memory-triage) above. This table provides the raw numbers behind those bullets._

| Indicator                         |    Value |
| --------------------------------- | -------: |
| Anonymous/generated classes       |      174 |
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
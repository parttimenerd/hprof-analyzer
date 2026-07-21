# Why Rust Was the Right Language for This Job

*Grounded in the actual source code of both the Rust and Java implementations.*

---

## The core argument in one paragraph

Both tools implement the same algorithms: SEMI-NCA dominator tree, VByte-delta-encoded inbound CSR, two-level address index, chunked forward CSR. The Java implementation (`hprof-redact`) is not naive — it uses primitive arrays throughout, Eclipse Collections primitive maps, and a careful manual lifecycle protocol with a `PhaseArrays` donation pool. And yet Rust uses **6.6× less RAM on small dumps and 1.4× less on the 33 GB production dump**, with a clear path to further improvement that Java cannot take. The reason is not algorithmic. The reason is that the techniques that matter most — compressing a 2 GB array mid-pipeline without a third file scan, physically returning memory to the OS while a loop is still running, and freeing temporary arrays at compiler-verified points inside a function — all require the language to give you deterministic, compile-time-enforced ownership of memory. Java's GC does not give you that. Off-heap Java (`Unsafe`, `ByteBuffer.allocateDirect`) technically gives you it, but at the cost of abandoning the type system entirely, which trades one problem for another.

---

## The numbers

Peak RSS with `/usr/bin/time -l` on Apple Silicon macOS. Both tools ran on the same dumps.

| Dump | File | Rust RSS | Java RSS | Ratio |
|------|------|----------|----------|-------|
| `dump_4_philosophers.hprof` | 23 MB | **45 MiB** | **300 MiB** | 6.6× |
| `dump_2_scala-doku.hprof` | 51 MB | **99 MiB** | **288 MiB** | 2.9× |
| Large real-world dump | 33.4 GiB | **14.65 GiB** | **20.32 GiB** | 1.4× |
| MAT on same 33.4 GiB dump | 33.4 GiB | — | **62.05 GiB** | 4.2× vs Rust |

Wall time on 33.4 GiB: Rust **13:21**, MAT **27:16**.

The gap on large dumps is 1.4× — not 6.6×. That is because the JVM baseline overhead (JIT compiled code, class metadata, G1 heap regions, thread stacks) costs a fixed ~200–300 MB regardless of dump size. On a 23 MB dump that fixed cost dominates. On a 33 GB dump it disappears into the noise. What remains is the structural gap: ~5.5 GB that Java cannot close.

The Java tool's own internal planning document (`suggestions.md`) puts the minimum achievable RSS at **~18.5 GB** after six non-trivial algorithmic refactors. Rust's actual peak is **~15 GB**. The 3.5 GB difference is entirely the language floor.

---

## The pipeline

Both tools do the same computational work in the same order:

```
Pass 1: read HPROF metadata, build id_map (address → dense index)
Pass 2: scan objects twice — degree count, then CSR fill
Compress cold arrays
RPO depth-first search
Inbound CSR transpose
SEMI-NCA dominator tree
Retained-size fold
Report building
```

The peak memory window is **between "inbound CSR transpose" and "retained-size fold"** — specifically during the RPO DFS phase. At 513M objects the Java tool peaks at **28.53 GB** there (from `suggestions.md` phase-by-phase table). Rust peaks at ~15 GB on comparable inputs.

What each phase needs simultaneously, at 513M objects where each `int[]` of N elements costs 1.96 GB:

| Java RPO phase | GB |
|---|---|
| Forward CSR targets (`fwdTargets`) | 6.68 |
| Forward CSR offsets | 1.96 |
| RPO order | 1.96 |
| DFS position, order, parent | 5.88 |
| Post-order | 1.96 |
| Inbound offsets + encoded stream | 4.46 |
| Always-live (shallowSizeDiv8 + classIndex + idMap + bucket) | 3.67 |
| **Total** | **28.53 GB** |

Rust at the same window: **~15 GB**. Every item in the list above has a Rust counterpart. The gap is in what Rust can *not have in memory at the same time*.

---

## The five things Rust can do that Java cannot

### 1. Compress cold arrays to ~33 MB while they are still needed later

The two per-object arrays `shallow` (shallow heap size per object) and `class_idx` (class identity per object) are built during pass 2. They are not needed again until the report phase — after the entire RPO → inbound → dominator peak window. Those arrays are each ~2 GB at 513M objects.

In Rust, they are compressed to ~33 MB zstd blobs immediately after pass 2 (`src/cvec.rs`, `src/main.rs:763–784`). The originals are dropped. The backing allocation returns to the allocator at the moment `CompressedU32::compress()` returns. The blobs sit in RAM across the entire peak window. Before the report phase they are decompressed once. No disk I/O. No extra file scan.

```rust
// src/main.rs line 763-764
let (mut g, mut inbound, shallow_c, class_idx_c, alloc_serial_c) =
    pass2::Pass2::build(input, p1, compress, &opts)?;
// shallow_c and class_idx_c are ~33 MB blobs. g.shallow and g.class_idx are empty.
```

The compression itself (`src/cvec.rs:55–57`) is zero-copy on little-endian hardware:

```rust
// SAFETY: [u32] and [u8] have no padding or provenance constraints;
// reinterpreting u32 memory as bytes is always well-defined.
let bytes = unsafe { std::slice::from_raw_parts(v.as_ptr() as *const u8, v.len() * 4) };
zstd::encode_all(bytes, 3)
```

The `Vec<u32>` backing the original array is fed to zstd directly — no intermediate copy, no `ByteBuffer`, no extra allocation.

**What Java does instead**: nulls the arrays and re-reads the HPROF file a third time (`HeapGraphBuilder.java:819`, `phaseA3()`). From `suggestions.md`: *"Refilled by phaseA3() after DOM, before RetainedSizes.compute(). Saves ~2.25 GB throughout the ~400s A2b/A2c/RPO/DOM window."* This works, but it costs an entire sequential file pass on a potentially multi-gigabyte file, and the free only happens when GC decides to collect — not at the semicolon.

**Why Java can't do what Rust does**: To compress an `int[]` and free the original in Java you need to (a) copy the `int[]` into a `byte[]` because `zstd4j` takes `byte[]`, not `int[]`, (b) call `System.gc()` and hope the GC actually collects the original before the next large allocation, and (c) accept that on a tight computation loop with no allocation pressure G1 may not run at all. The `hprof-views.sh` wrapper expresses this dependency explicitly with seven hand-tuned G1GC flags: `-XX:G1PeriodicGCInterval=20`, `-XX:MaxHeapFreeRatio=2`, `-XX:G1PeriodicGCSystemLoadThreshold=0.0`, and four others. Those flags are not optimization — they are mitigation for the fact that `null` in Java is a suggestion, not a contract.

---

### 2. Free a 2 GB array mid-function before the next allocation

In `src/dominator.rs`, the SEMI-NCA algorithm allocates three parallel arrays of size `count` (the number of reachable objects): `semi`, `ancestor`, `label`. At 513M objects each is ~2 GB. After Phase 1 (semidominator computation), `label` and `semi` are provably dead — they are never read again.

```rust
// src/dominator.rs:150–171
let mut idom_pre = ancestor;   // reuse ancestor's Vec<u32> — zero new allocation
drop(label);                   // 2 GB freed HERE, before Phase 2 reads anything
// ...
// Phase 2 loop runs here (reads semi[i] as upper bound on d)
// ...
drop(semi);                    // 2 GB freed HERE, before idom is allocated
// src/dominator.rs:171: probe("dominator: after drop(semi), before idom alloc")

let mut idom = vec![UNDEFINED; n + 1];  // allocates into the memory just freed
```

The comment in the code explains: *"Free it before allocating `idom` so the ~2GB (count\*4 @514M) region can back the new (n+1)\*4 `idom` array in place, rather than adding a fresh 2GB on top of the dominator-window peak."*

The borrow checker enforces that `semi` and `label` are not read after `drop()`. This is not a comment or convention — it is a compile error.

**What Java does**: `DominatorTree.java:163–172` sets `label = null` and donates `ancestor` to the `PhaseArrays` pool. This is the correct intent. But:

1. `label = null` does not free memory. It removes a GC reference. The array is freed when GC next runs. During a tight computation loop with no allocation pressure (the Phase 1 inner loop over 513M objects), G1 may not run at all between `label = null` and the `new int[reachable]` allocation — meaning both arrays exist simultaneously in RSS.

2. Nothing in the Java compiler verifies that `label` is not read after it is nulled. A future refactor could silently break the invariant.

3. `suggestions.md` documents a specific instance of this problem as improvement **N5** (`childTargets-early-null`): *"add `childTargets = null;` immediately after the `while (sp > 0)` DFS loop. Array is provably dead after the loop; the local reference prevents GC until method return."* This is a **1.96 GB saving** that is not realized in the current codebase specifically because Java extends local variable lifetimes to end-of-method.

The pattern repeats. Every explicit `drop()` in the Rust pipeline corresponds to a `x = null; System.gc()` in Java that may or may not work. In Rust, if `drop(semi)` is in the code, the memory is freed there. In Java, if `semi = null` is in the code, the memory might be freed there.

---

### 3. Return physical pages to the OS while a loop is still running

The inbound CSR transpose fills a large intermediate flat array (`inb_flat: ChunkU32`), then immediately reads it left-to-right to produce the VByte-encoded inbound stream. The flat array and the encoded stream must coexist briefly at peak. Without mitigation, their sum (~6.68 GB + ~4.38 GB) would be the binding peak.

`src/chunkvec.rs` splits `inb_flat` into 256 MB chunks. As the encode cursor advances past each chunk boundary, that chunk is freed:

```rust
// src/chunkvec.rs
pub fn free_below(&mut self, boundary: usize) {
    let last_chunk = boundary >> CHUNK_LOG;
    for c in 0..last_chunk {
        if !self.chunks[c].is_empty() {
            #[cfg(target_os = "linux")]
            unsafe {
                libc::madvise(ptr, len, libc::MADV_DONTNEED);
            }
            self.chunks[c] = Vec::new();
        }
    }
}
```

The `MADV_DONTNEED` call tells the Linux kernel to **immediately reclaim the physical pages** backing that chunk. This is not a soft hint — on Linux it is a hard contract. The pages are returned to the OS within microseconds. The peak becomes `|remaining(inb_flat)| + |built(inb_data)|` instead of `|inb_flat| + |inb_data|`, saving ~2 GB on the 34 GB dump.

**What Java cannot do**: There is no portable Java equivalent of `MADV_DONTNEED`. `ByteBuffer.allocateDirect()` with a `Cleaner` allows page-accurate freeing, but only as untyped off-heap memory — every read is `buf.getInt(offset)`, `Arrays.sort` is unavailable, and any exception in the cleanup path permanently leaks memory. The Java tool does not use off-heap memory. `PERF.md` explicitly lists "no mmap" as a constraint. `ChunkU32`'s page-return mechanism has no on-heap Java equivalent.

---

### 4. Stream compressed data without ever holding the decompressed buffer

`src/cvec.rs` provides `for_each_u32`, which decompresses the zstd blob through a 64 KiB fixed buffer, calling a callback for each decoded `u32`, without ever materializing the full decompressed `Vec<u32>`:

```rust
// src/cvec.rs
pub fn for_each_u32<F: FnMut(u32)>(&self, mut f: F) -> io::Result<()> {
    // Uses a fixed 64 KiB buffer internally.
    // Transient O(64 KiB), not O(n).
}
```

The histogram and class-resolution report phases read `shallow` and `class_idx` this way — consuming each value exactly once without the 4 GB decompressed array ever being in memory. Java's streaming APIs (`InputStream`, `Scanner`) could achieve something similar on a `byte[]` blob, but only for sequential one-time reads. Any API requiring random access or re-reading must decompress fully.

---

### 5. The two-level `IdMap`: 4 bytes per address, always, regardless of heap size

The address → dense-index table must store one entry per object in the dump. The naive choice is `HashMap<Long, Integer>` (~80–96 bytes per entry due to boxing — **~24 GB for 513M objects**, immediately unusable). The Java tool avoids this with a sorted `long[]` and binary search; at 513M objects this is a 4.11 GB array.

`src/id_map.rs` decomposes addresses into a few 64-bit block bases plus per-object `u32` offsets:

```rust
pub struct IdMap {
    block_base: Vec<u64>,   // one entry per 4 GB address block (typically 1–5)
    block_start: Vec<u32>,  // sentineled block index
    offsets: Vec<u32>,      // per-object offset within its block
    staging: Vec<u64>,      // temporary; freed by sort_and_dedup()
}
```

At 513M objects: `IdMap` = **2.06 GB** (half of `long[]`). The comment in `id_map.rs` (lines 1–8): *"A new block starts whenever the next sorted address is 2^32 or more beyond the current block base... this halves the per-object cost (4 bytes vs 8)."*

The Java tool has a compressed-OOPs fallback (`int[]` when addresses fit in 35 bits) that achieves the same cost — but **only when the JVM heap fits in 8 GB** (the compressed-OOPs threshold). For the customer dump that triggered most of this work, compressed OOPs are off. Rust's two-level layout saves 2 GB unconditionally.

Additionally, `id_map.rs` lines 302–393 stream the address table through a delta-VByte encoder without materializing the full `Vec<u64>`. The comment at line 320–324: *"WITHOUT reconstructing the 4.1 GB `addrs: Vec<u64>` (@514M) that used to be the binding compress-cold peak on top of the ~13 GB fwd CSR."*

---

## What the Java implementation does instead — and why it is harder

The Java tool is not naive. It makes serious attempts at all of the above:

| Problem | Java solution | Why it falls short |
|---------|--------------|-------------------|
| Cold array peak | Null + `phaseA3()` third scan | Requires re-reading the file; GC timing non-deterministic |
| Mid-function free | `x = null` + `System.gc()` | Advisory; compiler does not verify; local lifetimes extend to end-of-method |
| Physical page return | Seven G1GC tuning flags in wrapper script | Best-effort; no `MADV_DONTNEED` equivalent on-heap |
| Address table size | Sorted `long[]` + compressed-OOPs fallback | Falls back to 8 bytes/entry for heaps >8 GB |
| Zero-copy u32→u8 | Not possible; `byte[]` copy required | Adds allocation and copy to every compress call |
| Buffer reuse protocol | `PhaseArrays` donation pool | Convention, not enforcement; `suggestions.md` has 4+ examples where it is missed |

The `PhaseArrays` donation pool deserves elaboration. It is a two-slot register (`HeapGraph.java:362–399`) that passes reusable `int[N]` arrays between pipeline phases to avoid re-allocating 2 GB arrays. It is an explicit manual implementation of what Rust gives for free via ownership types. And `suggestions.md` records four separate improvements (N1, N2, N3, N5) that are missed donations or missed nullings — **each one a 1–2 GB saving that is sitting unrealized because the protocol is convention, not enforcement**.

In Rust, if you move an array into a function with `std::mem::take`, the compiler guarantees it is consumed. If you `drop()` a variable, the compiler guarantees it is not read again. There is no donation protocol to maintain and no missed nullings to find.

---

## Where Java cannot go at all

After all algorithmic improvements in `suggestions.md` are applied, the estimated minimum Java peak is **~18.5 GB**. The Rust tool achieves **~15 GB** today. The remaining 3.5 GB gap has three sources, none of which are available to Java:

**1. Zstd compression of cold arrays without a third file scan: ~2 GB net.**
Java can null the arrays — but to recover them it must re-read the file (`phaseA3`). Rust compresses in memory, decompresses on demand. The ~5 seconds compression time trades for zero I/O and 2 GB freed immediately and deterministically.

**2. `MADV_DONTNEED` during the inbound encode loop: ~1 GB.**
There is no on-heap Java equivalent. Off-heap Java (`ByteBuffer.allocateDirect`) achieves it, but at the cost of abandoning the type system for all operations on the array. The `hprof-redact` PERF.md rules this out explicitly.

**3. Deterministic mid-function drops of SEMI-NCA temporaries: ~0.5 GB.**
`drop(semi)` at `dominator.rs:170` frees 2 GB before `idom` is allocated. Java requires `semi = null` and a GC cycle to align. On a computation-heavy phase with minimal allocation, G1 does not run without explicit prompting. `System.gc()` is a hint.

These are not implementation gaps. They are language-level constraints. The Java tool is already a best-effort approximation of the Rust architecture, and its own planning documents record precisely where the approximation fails.

---

## The honest counterargument

**"You could use off-heap Java."** `ByteBuffer.allocateDirect()` or `sun.misc.Unsafe` gives you deterministic deallocation via `Cleaner`. In principle you can get deterministic `MADV_DONTNEED` behavior too. In practice: every array access becomes `buf.getInt(offset)`, you cannot use `Arrays.sort`, exceptions in cleanup paths permanently leak memory, you need either a JDK internal API or an external dependency (Agrona, Chronicle Map), and the resulting code is C with Java syntax and without Java's type safety. `hprof-redact/PERF.md` explicitly prohibits this approach ("no mmap"). Calling off-heap Java a solution is like calling JNI a solution — you have left the language.

**"Java has Project Valhalla."** Valhalla's value types will eventually allow `int`-like structs without object headers, closing the boxing gap. But Valhalla does not give you deterministic deallocation, `MADV_DONTNEED`, or compiler-verified ownership transfer. The structural gap in this analysis will remain.

**"The 1.4× gap on large dumps isn't that impressive."** On the 33 GB dump the gap is 14.65 GB vs 20.32 GB — a 5.67 GB saving. That is the difference between fitting the analysis on a 16 GB machine and requiring a 24 GB machine. For enterprise customers analyzing production dumps from 32 GB JVM heaps, it is the difference between the tool working and the tool OOMing.

---

## Summary

| Technique | Rust: what the language provides | Java: what you do instead |
|-----------|----------------------------------|--------------------------|
| Compress cold arrays, free originals | `CompressedU32::compress()` + implicit drop; zero-copy via `from_raw_parts` | Null + `phaseA3()` third file scan + GC hint |
| Free 2 GB array mid-function | `drop(semi)` — compile-time verified, immediate | `semi = null` — convention, GC-timing dependent, local lifetime extends to end-of-method |
| Physical page return mid-loop | `MADV_DONTNEED` on freed chunks | No on-heap equivalent; G1 flags partially mitigate |
| Address table: 4 B/entry always | `IdMap` two-level `Vec<u32>` offset blocks | `long[]` (8 B) unconditionally for heaps >8 GB |
| Buffer lifecycle protocol | Ownership types — compiler-enforced | `PhaseArrays` donation pool — convention, four known missed savings |
| Move semantics: consume builder, free fields | `std::mem::take` — compile-time enforced | Null fields manually, no verification |

Rust did not give this project better algorithms. Both implementations converged on the same algorithms independently. What Rust gave was **the ability to express the cost model of the pipeline in the type system** — so that the compiler, not the programmer, is responsible for ensuring that a 2 GB array does not exist in memory during the 90-second window when it is not needed.

---

## Source files

| File | What it shows |
|------|--------------|
| `src/cvec.rs` | Compressed cold arrays: zero-copy zstd, `for_each_u32` streaming |
| `src/chunkvec.rs` | Mid-loop chunk freeing + `MADV_DONTNEED` |
| `src/id_map.rs` | Two-level address index, streaming delta-VByte without materializing `Vec<u64>` |
| `src/dominator.rs:149–171` | `drop(label)`, `drop(semi)` before `idom` alloc; `idom_pre` reuses `ancestor` buffer |
| `src/main.rs:728–895` | Full pipeline ordering; `std::mem::take` on fwd CSR; compress/decompress sequencing |
| `hprof-redact/src/.../HeapGraphBuilder.java` | `phaseA3()`, seven `System.gc()` calls, `PhaseArrays` donation protocol |
| `hprof-redact/src/.../DominatorTree.java` | `label = null`, `donate(ancestor)` |
| `hprof-redact/PERF.md` | Five file passes, O(n²) sort bug, per-record allocation — the Java tool's own bottleneck list |
| `hprof-redact/suggestions.md` | Phase-by-phase RSS breakdown; minimum achievable ~18.5 GB; four missed nullings/donations |
| `hprof-views.sh` | Seven G1GC flags needed to coax the JVM into releasing memory |

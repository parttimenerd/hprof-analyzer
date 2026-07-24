# MAT Cache Generation (`mat caches`)

The `mat caches <hprof> <dir>` subcommand generates the 12 on-disk cache files that Eclipse MAT writes after its first parse of a heap dump. When these files are present and newer than the `.hprof`, MAT skips its own parser entirely and loads from cache — reducing open time from ~4 s to ~0.9 s on a 34 GB dump.

We generate all 12 files in a single pipeline pass through the dump, byte-identical to MAT's own output (with the documented exceptions below).

## Generated files

| File | Format | Content |
|------|--------|---------|
| `<dump>.idx.index` | LongIndex | Object address → MAT object id (sorted address table) |
| `<dump>.o2hprof.index` | LongIndex | MAT object id → byte offset of the object's record in the `.hprof` |
| `<dump>.o2c.index` | IntIndex | MAT object id → MAT class id |
| `<dump>.domIn.index` | IntIndex | MAT object id → immediate dominator MAT id |
| `<dump>.a2s.index` | LongIndex | MAT object id → shallow size |
| `<dump>.o2ret.index` | LongIndex | MAT object id → retained size |
| `<dump>.outbound.index` | IntArray1N sorted | Per-object outbound reference list |
| `<dump>.inbound.index` | IntArray1N sorted | Per-object inbound reference list |
| `<dump>.domOut.index` | IntArray1N unsorted | Per-object dominator-tree children |
| `<dump>.i2sv2.index` | Raw int+long pairs | Per-class retained size |
| `<dump>.threads` | Text | Thread stacks and local variables |
| `<dump>.index` | Java serialization | Master snapshot metadata (class cache, GC roots, loader labels) |

MAT's index reader expects the files named `<hprof-basename>.<suffix>` in the same directory as the `.hprof` file, or in a directory you point MAT at.

## Known differences from MAT's own output

These are intentional divergences that MAT accepts at load time:

- **`.index`**: The embedded hprof path string differs (our absolute path vs MAT's path). MAT re-resolves the path at open time; the difference is harmless.
- **`outbound.index` / `inbound.index`**: Our synthetic GC-root model differs slightly from MAT's — the virtual superroot edges are arranged differently. MAT loads the files without error; some root-path queries may return slightly different paths.
- **`a2s.index` / `o2ret.index`**: Objects that are GC roots but have no class stub (MAT adds a synthetic shallow size for its stub; we do not). Rare, affects only a handful of objects.
- **`i2sv2.index` / `threads`**: Content-equivalent but not byte-identical (different serialization ordering).

For `idx`, `o2c`, `domIn`, `o2hprof`: output is byte-identical to MAT's.

## Peak RSS budget on large dumps

Generating caches for a 34 GB heap dump (~513M objects, 1.65B references) is the hardest part. The pipeline is designed to stay under 20 GB peak RSS on a machine with 128 GB RAM. Key constraints:

- No mmap
- No temporary files
- Interleave each emit with the pipeline stage where the source data is naturally live

### Peak windows and what lives in them

```
pass1 scan          → peak ~16 GB  (tmp_addrs + payloads + id_map sort)
rpo_dfs             → peak ~18 GB  (fwd CSR + dfn + id_map compressed)
inbound scan        → peak ~19 GB  (inb_flat 6.3 GB + id_map + other)
dominator           → peak ~18 GB  (semi/ancestor/label + idom)
MatIdMap build      → peak ~14 GB
outbound emit       → peak ~19 GB  ← hardest window, see below
retained + rest     → peak ~18 GB
```

### Outbound emit: the hard window

During `emit_outbound_cb`, three large arrays are simultaneously live:

| Array | Size |
|-------|------|
| `fwd_tgt` (scatter-filled CSR targets) | 6.3 GB |
| `fwd_off` (CSR prefix-sum offsets) | 2.0 GB |
| `class_obj_ids` (per-object class dense id) | 2.0 GB |

That baseline is ~17.2 GB. The strategies below keep the peak near 19 GB:

**In-place MAT-id translation** (`main.rs`): Rather than building a per-object scratch `Vec<i32>` to translate dense→MAT ids before sorting, we translate directly inside `fwd_tgt[lo..lo+count]`. For objects with 100M+ outbound refs (large Java arrays), the scratch approach inflated peak RSS by ~1.8 GB. In-place eliminates that entirely.

**Deferred `class_obj_ids` restore** (`main.rs`): `class_obj_ids` is kept compressed across the outbound rescan (scatter-fill phase) and only restored after scatter completes. This avoids inflating the scatter peak by 2 GB.

**Streaming `CompressedU32::restore`** (`cvec.rs`): When decompressing a `Vec<u32>` from zstd, the naïve approach holds both the raw `Vec<u8>` (2 GB) and the output `Vec<u32>` (2 GB) simultaneously. We instead stream through a 64 KiB read buffer directly into the output Vec, keeping the transient O(64 KiB).

**zstd-compressed header spool** (`mat/int_index_1n.rs`): The IntArray1N sorted format requires the header (one i32 body-position per object, ~2 GB raw) to be written *after* the body. We cannot hold a plain `Vec<i32>` without blowing the budget. Instead we stream header values through a zstd level-3 encoder (`Encoder<Vec<u8>>`) as the body is written. Header positions increase slowly (average delta ~3–4, interspersed with 0-holes for empty entries), so the compressed blob is a small fraction of 2 GB. After the body is done, `fwd_tgt` and `class_obj_ids` are dropped, and the blob is decompressed streaming through a 64 KiB buffer into the header `IntIndexStreamer`.

**`malloc_trim` after scatter** (`main.rs`): After the HPROF rescan that scatter-fills `fwd_tgt`, the compressed `id_map` blob is freed. On Linux with glibc, freed pages stay in the allocator's free list and still count toward VmHWM unless explicitly returned. `malloc_trim(0)` returns them to the OS before `class_obj_ids` is restored.

**Streaming id_map decompression** (`id_map.rs`): Reconstructing the `old_to_mat` / `sorted` vectors from compressed vbyte-delta storage uses a 64 KiB read buffer rather than materializing the full ~2 GB intermediate byte Vec.

## `o2hprof` implementation

MAT needs the byte offset in the `.hprof` for every object to support direct record access. We track this in pass1: a `bytes_consumed` counter on `HprofReader` is sampled at the start of each `INSTANCE_DUMP`, `OBJ_ARRAY_DUMP`, `PRIM_ARRAY_DUMP`, and `CLASS_DUMP` record. After pass1's address-sort permutation is applied, `p1.hprof_offsets[dense_id]` is the `.hprof` file offset for that object. For gzipped dumps, offsets are positions in the decompressed stream (same as MAT's behavior).

## CLI

```
# Generate cache alongside the dump file
hprof-analyzer mat caches dump.hprof /path/to/dump-dir/

# Generate with explicit MAT binary (for auto-detecting the parser id in .index)
hprof-analyzer mat caches dump.hprof /path/to/dump-dir/ --mat-binary /opt/mat/MemoryAnalyzer

# Generate inline during full analysis
hprof-analyzer analyze dump.hprof --mat /path/to/dump-dir/
```

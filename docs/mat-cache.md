# Speeding up Eclipse MAT with hprof-analyzer

Eclipse MAT parses a 34 GB heap dump in ~4 seconds and writes 12 cache files
alongside the `.hprof`. On the next open it reads from cache and loads in ~0.9 s.
The catch: MAT's own parser peaks at **~55 GB RSS** while writing those caches
on the same 34 GB dump.

hprof-analyzer generates the same 12 cache files in a single pipeline pass,
peaking at **~19 GB RSS** — less than half of MAT's peak. Once the cache files
are present, MAT opens instantly at the same low memory cost regardless of who
generated them.

## Workflow

```
# Step 1: generate the cache (fast, low RSS)
hprof-analyzer mat caches dump.hprof /path/to/dump-dir/

# Step 2: open in MAT as usual — it detects the cache and skips its parser
# File → Open Heap Dump → pick dump.hprof in the same dir
```

MAT detects the cache automatically: if all index files are present and newer
than the `.hprof`, it prints "Reopening parsed heap dump file" and skips the
parse entirely.

If you also want hprof-analyzer's own report, you can generate both in one pass
(single hprof read, shared pipeline) with the `--mat` flag on `analyze`:

```
hprof-analyzer analyze dump.hprof --mat /path/to/dump-dir/
```

This is cheaper than running `analyze` then `mat caches` separately (two full
hprof parses vs one).

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

MAT expects files named `<hprof-basename>.<suffix>` in the same directory as
the `.hprof`. The `<dir>` argument to `mat caches` is where the `.hprof` lives
(or a separate output directory if you prefer to keep them together).

## Known differences from MAT's own output

These are intentional divergences that MAT accepts at load time without error:

- **`.index`**: The embedded hprof path string differs (our absolute path vs MAT's path). MAT re-resolves the path at open time; harmless.
- **`outbound.index` / `inbound.index`**: Our synthetic GC-root model differs slightly — the virtual superroot edges are arranged differently. Some root-path queries may return slightly different paths.
- **`a2s.index` / `o2ret.index`**: MAT adds a synthetic shallow size for GC-root stubs; we do not. Affects a handful of objects.
- **`i2sv2.index` / `threads`**: Content-equivalent but not byte-identical (different serialization ordering).

`idx`, `o2c`, `domIn`, `o2hprof`: byte-identical to MAT's output.

## CLI reference

```
# Standalone cache generation
hprof-analyzer mat caches <hprof> <dir> [--mat-binary <path>] [--trace-rss]

# Combined analysis + cache generation (single hprof parse)
hprof-analyzer analyze <hprof> --mat <dir> [--mat-binary <path>]
```

`--mat-binary` points to the `MemoryAnalyzer` executable. When given,
hprof-analyzer derives the MAT plugins directory from it and embeds the correct
parser ID string in `.index`. Without it, hprof-analyzer auto-detects from
common install locations; if detection fails it falls back to a fixed default
that works with MAT 1.13.x.

## Why the RSS is so much lower

MAT's writers materialise full `int[N]` / `long[N]` arrays in Java heap — for
a 34 GB dump that means allocating several arrays of 500M–1.65B elements at
once inside the JVM. hprof-analyzer instead:

- **Interleaves each emit** with the pipeline stage where its source data is
  already live, rather than deferring everything to the end
- **Streams per page** (1M ints at a time), never holding the full value array
- Applies several targeted tricks in the hardest window (outbound emit, where
  `fwd_tgt` 6.3 GB + `fwd_off` 2 GB + `class_obj_ids` 2 GB are all live):
  - **In-place MAT-id translation** inside `fwd_tgt` — no per-object scratch Vec
  - **Deferred `class_obj_ids` restore** — kept compressed across the scatter phase
  - **Streaming decompression** for `CompressedU32::restore` — 64 KiB read buffer,
    no intermediate 2 GB byte Vec
  - **Delta-encoded zstd header spool** — the IntArray1N sorted format requires
    the 2 GB header to be written *after* the body; we stream it through a zstd
    encoder with delta encoding (deltas are small integers ~3–5, compress to
    ~25 MB) instead of holding a raw `Vec<i32>`
  - **`malloc_trim`** after scatter to return freed id_map pages to the OS before
    restoring `class_obj_ids`

The measured peak on a 34 GB production dump is **19.2 GB** vs MAT's ~55 GB.

## `o2hprof` implementation detail

MAT needs the byte offset in the `.hprof` for every object to support direct
record access. We track this in pass1: a `bytes_consumed` counter on
`HprofReader` is sampled at the start of each object record (`INSTANCE_DUMP`,
`OBJ_ARRAY_DUMP`, `PRIM_ARRAY_DUMP`, `CLASS_DUMP`). After pass1's address-sort
permutation, `p1.hprof_offsets[dense_id]` is the file offset for that object.
For gzipped dumps, offsets are positions in the decompressed stream — same
convention as MAT.

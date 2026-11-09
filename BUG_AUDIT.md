# hprof-analyzer Bug Audit Report

Comprehensive code audit covering OQL (parser, planner, executor, stage runner,
jsonify, refwalk, autocompletion), report generation (build, triage, model,
render_md, render_graphs, format), core analysis (dominator, retained, cache,
diff, pass1, pass2), data structures (cvec, id_map, chunkvec, vbyte),
infrastructure (MCP, HTTP server, redaction, secrets, HTML, CLI), and
deep infrastructure (cache serialization, secret detection patterns, self-update
security, HPROF reader robustness, compressed vectors, retained-size computation).

**Audited on:** 2026-11-09  
**Total findings:** ~220 across 5 rounds (after removing false positives)

---

## Quick-reference: Remaining Critical & High severity bugs

| # | Area | File | Description | Fix hint |
|---|------|------|-------------|----------|
| 1 | MCP server | `mcp/mod.rs:385` | Path traversal in `load_dump`/`redact` | Validate path extension + allow-list root dirs |
| 2 | Stage runner | `stage_runner.rs:2095` | Raw heap addr stored as dense index in `ObjRef.index` | Store sentinel `u64::MAX`; defer resolution |
| 3 | OQL parser | `parse.rs:830` | `INSTANCEOF` silently discards LHS attribute | Extend `Predicate::InstanceOf` to carry subject |

---

## 1. OQL Parser (`src/query/parse.rs`, `ast.rs`, `optimize.rs`)

### P-02 `Predicate::InstanceOf` silently discards LHS — High
**Line:** ~830  
**Bug:** `WHERE a INSTANCEOF Foo` and `WHERE b INSTANCEOF Foo` produce identical AST; the subject expression is dropped.  
**Fix:** Extend `Predicate::InstanceOf(String)` to `Predicate::InstanceOf { subject: Attr, class: String }`, update executor.

---

## 2. OQL Executor (`execute.rs`, `run.rs`, `plan.rs`)

### E-11 Wrong late op pushed for `@inbounds`/`@outbounds` in WHERE — Medium
**Line:** `plan.rs:1579`  
**Bug:** `ResolveClassMetadata` pushed instead of edge-related op.  
**Fix:** Add a dedicated `StageOp::ResolveEdgeCounts`.

### E-12 `row_address` for IN-subquery: ObjRef index vs address mismatch — Medium
**Line:** `run.rs:2491`  
**Bug:** `@objectId` inner + `@objectAddress` outer always fails to match.  
**Fix:** Reject `@objectId` in IN-subqueries, or resolve to address via lookup.

---

## 3. OQL Stage Runner (`stage_runner.rs`)

### S-03 Raw heap address stored as dense index in `ObjRef.index` — High
**Line:** ~2095  
**Bug:** `FieldValue::Ref(addr)` stores address in `index` field; downstream lookups into `retained[]`/`shallow[]` use wrong indices.  
**Fix:** Store `u64::MAX` sentinel; consumers must perform addr→dense lookup.

---

## 4. OQL Jsonify, Refwalk, Complete

*(No open bugs — all findings resolved.)*

---

## 5. Report Generation (`build.rs`, `triage.rs`, `model.rs`, `format.rs`)

### R-04 Package tree uses top-level-only accumulator — Medium
**Line:** `build.rs:5029`  
**Bug:** `class_tl` accumulated only over top-level dominators; `biggest_classes` uses all reachable.  
**Fix:** Merge `class_tl` accumulation into the all-objects loop.

### R-05 Triage score can overflow u32 on large heaps — Medium
**Line:** `triage.rs` (score computation)  
**Fix:** Widen all intermediate terms to `u64`.

---

## 6. Report Rendering (`render_md.rs`, `render_graphs.rs`, `anchors.rs`)

### RR-02 6 sections missing from `--md-graphs` output — High
**Line:** `render_graphs.rs:10–50`  
**Bug:** Finalizer Referents, Collection Retained Heap, Resource Handles, Dup Prim Arrays, Boxed Numbers, Header Overhead all omitted.  
**Fix:** Add the missing `render_*` calls in `render_markdown_graphs` and `render_system_overview_graphs`.

### RR-03 "Biggest Classes" table omits `"% Heap"` column — Medium
**Line:** `render_md.rs:1875`  
**Fix:** Add column `"% Heap"` with `fmt_pct(pct_of_heap(row.retained, total_shallow))`.

### RR-04 Depth histogram renders empty table when all rows are a chain — Medium
**Line:** `render_md.rs:956`  
**Fix:** When `chain_start == Some(0)`, show at least the first row.

### RR-06 Plain-MD ToC puts "Top Retainers" in wrong position — Low
**Line:** `render_md.rs:575`  
**Fix:** Move `TopRetainers` bullet before `CollectionContents`.

### RR-07 Graphs retention concentration drops absolute byte count — Low
**Line:** `render_graphs.rs:698`  
**Fix:** Add `"Retained"` column alongside the bar.

---

## 7. Core Analysis

*(No open bugs — all findings resolved.)*

---

## 8. Pass2 / Field Decoding / Sizing

### F-03 `DirectByteBuffer.capacity` read as i32, field may be long — Medium
**Line:** `fielddecode.rs:905`  
**Fix:** Retain the type from `field_offset` and dispatch by `HprofType`.

### F-06 `DupPrimCollector` stores `bytes.len()` as shallow size — Medium
**Line:** `dup_prim_arrays.rs:104`  
**Bug:** Missing object header → wasted bytes systematically under-counted.  
**Fix:** Pass true `shallow` size from caller, or compute with `prim_array_shallow`.

### F-08 Unsafe raw-pointer aliasing in field decode loop — Low
**Line:** `fielddecode.rs:1201`  
**Fix:** Clone the layout vec before the loop: `let layout = self.obj_field_layout[&class_id].clone();`.

---

## 9. Infrastructure (MCP, HTTP, Secrets, Redact, HTML, CLI)

### I-01 Path traversal in MCP `load_dump`/`redact` — **Critical**
**Line:** `mcp/mod.rs:385, 1383`  
**Fix:** Canonicalize path, validate extension (.hprof), optionally allow-list root dirs.

### I-08 Password/secret/token patterns too broad — Medium
**Line:** `secrets.rs:90`  
**Fix:** Require minimum entropy or exclude sentinel values (null, false, true, N/A).

### I-09 Secret dedup drops additional locations — Low
**Line:** `secrets.rs:216`  
**Fix:** Merge new locations into existing finding instead of dropping.

### I-11 `compare_dumps` runs analyses sequentially — Low
**Line:** `mcp/mod.rs:1468`  
**Fix:** Use `tokio::join!` on the two `spawn_blocking` calls.

### I-13 MCP `query` tool TOCTOU: session may change between path extraction and query — Medium
**Line:** `mcp/mod.rs:797`  
**Fix:** Add session-epoch counter; detect change and return retry error.

## 10. Data Structures (cvec, id_map, vbyte, chunkvec)

### D-04 `CompressedU32::get_at` cannot short-circuit — Medium
**Line:** `cvec.rs:228`  
**Fix:** Refactor `for_each_u32` to accept `FnMut(u32) -> ControlFlow<()>`.

### D-05 `--obj-graph` tier: `--size` unconditionally overwrites `report_size` — Medium
**Line:** `main.rs:2323`  
**Fix:** Only apply `--size` override when explicitly specified by the user.

---

---

# Round 3 — Additional Findings (REPL, query server, triage, diff/MAT, frontend)

Files newly audited: `repl.rs` (342K), `server.rs` (73K), `runflags.rs`, `histogram.rs`, `stringvals.rs`, `viz.rs`, `diff/` (117K), `mat/` (173K), `triage.rs` (287K deep re-read), `shell.js` (263K), `App.tsx` (730K), `charts.tsx` (62K).

---

## 11. OQL REPL (`repl.rs`)

*(No open bugs — all findings resolved.)*

---

## 12. OQL Query Server (`server.rs`)

### SRV-03 (Medium) All errors return HTTP 400, including internal/serialization failures
**Lines 528–532.** `{"ok": false, "error": {"kind": "internal"}}` returns status 400 (Bad Request) instead of 500 (Internal Server Error).
**Fix:** Map `error.kind == "internal"` → HTTP 500.

### SRV-04 (Medium) `store_plan` re-parses OQL redundantly (TOCTOU)
**Lines 513–527.** After `run_query_json` succeeds, `store_plan` re-parses and re-plans the same OQL from scratch to cache it, doubling parse time and creating a window where the cached plan could differ from the one that ran.
**Fix:** Cache the plan that was already built by `run_query_json`, not a freshly re-parsed one.

---

## 14. Diff / MAT Compatibility

### MAT-01 (High) `addr_at_mat` always returns 0 for thread objects
**Lines 521–526 in `mat/mod.rs`.** All thread objects get address `0x0` in the MAT index, so MAT cannot navigate to thread objects.
**Fix:** Pass the actual thread object address from the report's thread-info entries.

### MAT-02 (High) `ClassImpl` address is 0 → `class_info` lookup always misses
**Lines 792–808.** `ClassImpl { address: 0, … }` means MAT's class→info map never finds any class, breaking the entire class hierarchy view.
**Fix:** Use the HPROF class object address from `class_map`.

### MAT-03 (High) GC roots map written empty
**Lines 911–919.** The GC roots `HashMap` is created empty and never populated before serialization. MAT sees zero GC roots → all objects appear unreachable.
**Fix:** Populate the map from the report's `gc_roots` data before writing.

### MAT-04 (Medium) `block_data` panics for payloads ≥ 256 bytes
**Lines 385–388 in `mat/serial.rs`.** `block_data` writes `data.len() as u8`, which panics (in debug) or wraps (in release) for payloads ≥ 256 bytes.
**Fix:** Split into 255-byte chunks with continuation: `for chunk in data.chunks(255) { w.write_all(&[chunk.len() as u8])?; w.write_all(chunk)?; } w.write_all(&[0])?;`.

### MAT-05 (Medium) `utf_bytes` silently truncates strings > 65535 bytes
**Lines 191–195 in `mat/serial.rs`.** Java serialization's modified UTF-8 length is a `u16`. Strings longer than 65535 bytes are truncated without warning.
**Fix:** Use the long-form UTF-8 encoding (`TC_LONGSTRING` + `u64` length) for oversized strings, or truncate with a trailing `…` and log a warning.

### MAT-06 (Medium) `int_index_1n` delta encoding: `as i32` cast unchecked
**Lines 286–290.** Same pattern as the `cache.rs` delta-encode bug: large u32 deltas silently overflow i32.
**Fix:** Use zigzag encoding or saturate with an error.

### DIFF-01 (Medium) `find('%')` parses class names with `%` incorrectly
**Line ~401 in `diff/parse.rs`.** Class names containing `%` (rare but legal in obfuscated code) are split at the wrong position. Use `rfind('%')`.

---

## 15. Triage Deep Re-audit (`triage.rs`)

### TRI-04 (Medium) `HashCollisionHotspot` off-by-one: `lower_ratio_bp` used instead of `upper_ratio_bp`
**Lines 2364–2367.** The bucket's lower bound is compared against the threshold instead of the upper bound, making the rule one bucket too lenient.
**Fix:** Replace `lower_ratio_bp` with `upper_ratio_bp` in the comparison.

### TRI-05 (Medium) `InternedStringBloat` false positive: JNI Global roots ≠ String.intern()
**Lines 2115–2123.** The rule assumes JNI Global roots indicate `String.intern()` usage, but JNI Global roots also include framework/native references unrelated to interning.
**Fix:** Only fire if the String population is significantly dominated by JNI Global roots AND the Strings have duplicate values.

### TRI-07 (Medium) `HeadlineRetainer` inconsistent percentage basis
**Lines 688, 711.** Some percentage calculations use `retained` as the denominator, others use `shallow`, within the same triage rule. The percentages are not comparable.
**Fix:** Use `total_retained` consistently within this rule.

### TRI-08 (Medium) `WorkerPoolRetention` division without `== 0` guard
**Lines 3004, 3032.** Integer division by pool size without checking for zero.
**Fix:** `if pool_size == 0 { continue; }`.

### TRI-09 (Medium) `ShutdownHookAccumulation` false positive on every server JVM
**Lines 4231–4238.** Path 2 fires whenever shutdown hooks exist, which is true of virtually every JVM running a web server, producing noise.
**Fix:** Raise the threshold or require shutdown hook retained bytes > a meaningful floor (e.g., 1 MB).

### TRI-10 (Medium) `FixedPerObjectOverhead` reports headers as "recoverable bytes"
**Lines 2338–2348.** Object headers are mandatory JVM overhead; reporting them as "recoverable" is misleading.
**Fix:** Rename to "structural overhead" and remove from the "recoverable" category.

---

## 16. Browser Frontend (`shell.js`, `App.tsx`, `charts.tsx`)

### FE-01 (High) `Nav` IntersectionObserver stale closure — empty deps
**`App.tsx` ~line 394.** The observer effect has `[]` deps, capturing a stale `items` snapshot. New sections added after initial render are never observed.
**Fix:** Add `items` (memoized) to the dependency array.

### FE-02 (High) Flame graph child cells not positioned absolutely → all left-aligned
**`charts.tsx` ~lines 248–254.** Child cells use `width: N%` but no `left` offset, so every level renders from the left edge regardless of the parent's position.
**Fix:** Add `position: absolute; left: ${cell.startPct}%` to child cells, with `position: relative` on the level container.

### FE-03 (High) Context-menu row lookup uses DOM order, not DataTable sort order
**`App.tsx` ~lines 251–260.** After sorting a column, `querySelectorAll(".rdt_TableRow").indexOf(rdtRow)` returns the visual index, but `visible[rowIndex]` uses the original insertion order. The wrong row object is passed to the handler.
**Fix:** Use a `data-row-id` attribute or the DataTable's own row-click callback to identify the row.

### FE-04 (High) `domTree.tsx` pruning removes nodes but keeps their edges → d3-dag crash
**`domTree.tsx`.** Pruning to depth 1 removes nodes but not their edges. d3-dag throws when an edge references a missing node ID.
**Fix:** Filter edges to keep only those where both `source` and `target` are in the surviving node set.

### FE-05 (Medium) `ExecSummaryCard` divides by `leaks.total_shallow` instead of reachable heap
**`App.tsx` ~line 629.** `topRetainsPct` uses the leak-suspect subset as the denominator, inflating the displayed percentage.
**Fix:** Use `report.overview.total_shallow`.

### FE-06 (Medium) `FlatTreemap` `origIdx` via `findIndex` breaks on duplicate slice names
**`charts.tsx` ~line 117.** Two slices with the same label always select the first one's index.
**Fix:** Pass a stable numeric index through the hierarchy instead of using name-based lookup.

### FE-07 (Medium) `ZoomableTreemap` `pathLabels` is a new array every render → `extras` memo always recomputes
**`charts.tsx` ~lines 329–332.** `path.map(…)` creates a fresh array reference, defeating `useMemo`.
**Fix:** `const pathLabels = React.useMemo(() => path.map(n => getLabel(n)), [path, getLabel]);`.

### FE-08 (Low) `downloadQueryCsv` `a.click()` without DOM attachment fails in Firefox
**`App.tsx`.** Firefox requires the `<a>` element to be in the DOM for `click()` to trigger a download.
**Fix:** `document.body.appendChild(a); a.click(); document.body.removeChild(a);`.

### FE-09 (Low) IntersectionObserver `rootMargin` sums > 100% → intersection zone is always empty
**`App.tsx` ~lines 394–417.** `-40% 0px -55% 0px` = 95% inset, leaving a 5% sliver that no heading can reliably intersect. Active-section detection relies entirely on the `lowestAbove` fallback.
**Fix:** Change to `-20% 0px -70% 0px` for a practical 10% intersection zone.

---

## Notes

- **False positives removed:** ~15 findings from the initial audit were reclassified as correct after deeper analysis (e.g., `retained.rs` vroot rollup, `dominator.rs` Phase 2 NCA, `scan.rs` ROOT_THREAD_OBJ skip size).
- **Severity scale:** Critical = wrong results or panic on valid input; High = wrong results on edge cases or panic on malformed input; Medium = incorrect behavior unlikely to cause data loss; Low = code quality / performance / cosmetic.
- **Fix hints** are provided inline with each finding above. Many fixes are one-line changes; the most impactful ones (E-01, P-10, S-02, C-02, R-01) should be prioritized.
- **Round 3 priority fixes:** TRI-01/02/03 (division-by-zero panics in triage), MAT-01/02/03 (MAT export completely broken), FE-02/03 (visible frontend bugs).
- **Round 4 priority fixes:** RED-04 (redaction stream desync on unknown type codes), UPD-02 (no signature verification on self-update), IDMAP-05 (carry buffer OOB write / UB), CVEC-01 (big-endian data corruption), RET-01 (u32 depth overflow → OOM/panic), SEC-08 (masking exposes 8 of 9 chars), CACHE-05 (non-atomic writes corrupt cache).

---

## Round 4 — Deep audit of core infrastructure (cache, secrets, id_map, cvec, update, reader, retained, sweep, redact)

Audited: `src/cache.rs`, `src/secrets.rs`, `src/id_map.rs`, `src/cvec.rs`, `src/update.rs`, `src/reader.rs`, `src/retained.rs`, `src/sweep.rs`, `src/redact.rs`

### Quick-reference: Round 4 Critical & High severity bugs

| # | ID | File | Severity | Description |
|---|-----|------|----------|-------------|
| 1 | RED-04 | `redact.rs:768` | High | Unknown HPROF type code falls back to 1 byte → stream desync, incomplete redaction |
| 2 | RED-03 | `redact.rs:96–158` | High | `START_THREAD` names never added to structural IDs; intent undocumented |
| 3 | UPD-02 | `update.rs:286` | High | Downloaded binary has zero integrity/signature verification |
| 4 | UPD-01 | `update.rs:372` | High | `read_to_end` with no size cap → unbounded allocation / OOM |
| 5 | IDMAP-05 | `id_map.rs:457–510` | High | Vbyte carry buffer `[u8;16]` — no bounds check → stack OOB write (UB) |
| 6 | IDMAP-02 | `id_map.rs:240` | High | `offsets.len() as u32` silently wraps at 4B+ objects → all lookups fail |
| 7 | CVEC-01 | `cvec.rs:399` | High | `CompressedU64::compress` uses native byte order but `restore` uses LE → silent corruption on big-endian |
| 8 | RET-01 | `retained.rs:156` | High | `(stk.len()+1) as u32` wraps at depth ≥ 2³² → panic or OOM |
| 9 | CACHE-05 | `cache.rs:357` | High | Non-atomic writes; crash mid-write leaves corrupt-but-valid cache |
| 10 | CACHE-01 | `cache.rs:126` | High | `pass1.bin.zst` / `report.json.zst` validated only by `.exists()`, no version check |
| 11 | CACHE-06 | `cache.rs:508` | High | `delta as i16` truncates `fwd_field_idx` deltas; no read function exists |
| 12 | SEC-04 | `secrets.rs:72` | High | DB connection URL patterns miss password-only auth (`postgres://:secret@host`) |
| 13 | SEC-08 | `main.rs:1119` | Medium | `mask_secret` exposes 8 of 9 chars for short secrets |
| 14 | SEC-01 | `secrets.rs:88` | High | Credit card pattern fires on any 4×4 digit string (no Luhn check) |

---

### 10. Cache (`src/cache.rs`)

#### CACHE-01 `pass1.bin.zst` / `report.json.zst` validated only by `.exists()` — High
**Lines:** 126–127, 143–191
**Bug:** Neither file carries a magic/version header. `is_valid()` checks only `.exists()`. A stale `pass1.bin.zst` from a schema change silently deserializes wrong data. A zero-byte file from a crash also passes.
**Fix:** Write a 4-byte magic + 2-byte version prefix into `pass1.bin.zst`; verify on read. Same for `report.json.zst`.

#### CACHE-02 `class_names.json.zst` not covered by `is_valid()` — High
**Lines:** 124–133, 155–169
**Bug:** `class_names.json.zst` is required for analysis but `is_valid()` never checks for its existence. A partial cache missing this file passes validation; reading it returns `Ok(None)` and the caller operates with empty class names.
**Fix:** Add `class_names.json.zst` existence check to `is_valid()`.

#### CACHE-03 Array count `n as u8` truncates silently for n ≥ 256 — Medium
**Line:** 360
**Bug:** `write_indexed_file` stores the array count as `u8`. If `n >= 256`, it wraps silently. Read side parses the wrong count and returns `None` for valid data.
**Fix:** Assert `n <= 255` or return an error.

#### CACHE-04 `len as usize` truncates on 32-bit targets — Medium
**Line:** 438
**Bug:** Frame length stored as `u64`, cast to `usize`. On 32-bit targets (WASM), frames > 4 GiB wrap silently → wrong `read_exact` length → corrupt data.
**Fix:** Use `usize::try_from(len)?`.

#### CACHE-05 Non-atomic writes: crash mid-write leaves corrupt but valid-looking cache — High
**Lines:** 357–372, 473–476
**Bug:** `File::create` truncates immediately. No write-to-temp-then-rename pattern. A crash between truncation and completion leaves partial files that pass `check_magic` / `.exists()`. Also no `sync_all()` before return.
**Fix:** Write to `.tmp` file, `sync_all()`, then `fs::rename()`.

#### CACHE-06 `delta as i16` truncates `fwd_field_idx` deltas; no read function exists — High
**Lines:** 508–518
**Bug:** `zstd_compress_delta_u16` truncates i32 deltas to i16. Field-index jumps > 32767 silently wrap. Additionally, no `read_delta_u16_array` function exists — data is written but permanently unreadable from cache.
**Fix:** Store deltas as i32 (not i16). Add the missing read helper.

#### CACHE-07 `dump_hash` mtime fallback to 0 creates hash collisions — Medium
**Lines:** 594–602
**Bug:** When `modified()` fails (WASM, network FS), mtime defaults to 0 silently. Two dump files with identical first-64-bytes and file size produce the same hash. Also `d.as_nanos() as u64` truncates u128.
**Fix:** Return error on unavailable mtime instead of falling back to 0. Use `try_into().unwrap_or(u64::MAX)` for the nanos cast.

#### CACHE-08 TOCTOU between `.exists()` and `File::open` — Low
**Lines:** 381, 397, 145/163/182
**Bug:** Every read path checks `.exists()` then `File::open()`. Between them, another process can delete the file. No file locking.
**Fix:** Remove `.exists()` checks; handle `ErrorKind::NotFound` from `File::open` directly.

---

### 11. Secret Detection (`src/secrets.rs`, `build.rs`, `src/main.rs`)

#### SEC-01 Credit card pattern has no Luhn check — High (false positives)
**Line:** secrets.rs:88
**Bug:** Pattern `[0-9]{4}[- ][0-9]{4}[- ][0-9]{4}[- ][0-9]{4}` matches version strings, order IDs, tracking numbers. No Luhn validation.
**Fix:** Add post-match Luhn check, or at minimum require a card-type prefix (`4…`, `5[1-5]…`, `3[47]…`).

#### SEC-02 Property-value patterns: `[Kk]ey` matches "monkey", "donkey", "Turkey" — High (false positives)
**Lines:** secrets.rs:94–96
**Bug:** `(?:[Aa]pi[_.-]?)?[Kk]ey[= :]+…` has no word boundary. Any string containing "key" with `=` triggers it.
**Fix:** Add `\b` word boundary before `[Kk]ey`.

#### SEC-03 HTTP Basic auth fires on any base64 string ≥ 16 chars — High (false positives)
**Line:** secrets.rs:83
**Bug:** `Basic [A-Za-z0-9+/]{16,}={0,2}` matches `BASIC_TYPE`, log messages, enum names containing "Basic". No check that decoded base64 contains `:`.
**Fix:** Post-match base64-decode and verify `:` separator.

#### SEC-04 DB connection URLs miss password-only auth — High (false negatives)
**Lines:** secrets.rs:72–77
**Bug:** `postgres(?:ql)?://[^:/@]+:[^/@]{6,}@` requires a username. URLs like `postgresql://:secret@host/db` (empty username, password-only auth) are missed.
**Fix:** Make the `user:` part optional: `(?:[^:/@]*:)?`.

#### SEC-05 URL query-param patterns use `.*` prefix → O(n²) backtracking — High (performance)
**Lines:** secrets.rs:85–86
**Bug:** `.*[?&](?:api[_-]?key|…)=…` scans the entire string. With long URLs (2000+ chars) and 460+ patterns, this is O(n²).
**Fix:** Replace `.*` with `[^?]*` or drop it (patterns are already full-string anchored).

#### SEC-07 TSV output doesn't escape tabs/newlines — Medium
**Line:** main.rs:1072–1076
**Bug:** A heap string containing `\t` or `\n` breaks TSV structure.
**Fix:** Strip or replace control characters before output.

#### SEC-08 `mask_secret` exposes 8 of 9 chars for short secrets — Medium
**Lines:** main.rs:1119–1128
**Bug:** For a 9-char secret `ABCDEFGHI`, mask shows `ABCD****FGHI` — only 1 char hidden. Short API tokens/passwords are nearly fully exposed.
**Fix:** Require minimum 8 hidden characters; fully mask if total length ≤ 16.

#### SEC-09 `adapt_regex` strips `\b` globally including inside character classes — Medium
**Line:** build.rs:211
**Bug:** Blanket `s.replace(r"\b", "")` also removes `\b` from inside `[…]` (where it means backspace), changing regex semantics.
**Fix:** Use regex-aware stripping that only removes `\b` outside character classes.

#### SEC-10 JWT pattern rejects padded base64url tokens — Medium
**Line:** secrets.rs:79
**Bug:** `[A-Za-z0-9_\-]` excludes `=`. Many JWT libraries emit padded tokens — these are missed.
**Fix:** Add `=*` after each base64url segment.

---

### 12. IdMap (`src/id_map.rs`)

#### IDMAP-02 `block_start` overflow: `offsets.len() as u32` wraps at 4B objects — High
**Lines:** 240, 256, 279, 283
**Bug:** At 2³² objects, `offsets.len() as u32` wraps to 0. `block_start[b+1] < block_start[b]` → empty slice → all lookups return `None`.
**Fix:** Assert `offsets.len() <= u32::MAX` or switch `block_start` to `Vec<usize>`.

#### IDMAP-03 `addr_at` panics on empty map — Medium
**Lines:** 315–319
**Bug:** `partition_point(…) - 1` underflows to `usize::MAX` if `block_start` is empty. Then `block_base[usize::MAX]` panics.
**Fix:** Add explicit bounds check: `assert!(i < self.offsets.len())`.

#### IDMAP-04 `from_compressed` Codec::None length check is `debug_assert` only — Medium
**Lines:** 446–450
**Bug:** In release builds, blob/length mismatch causes silent incomplete map (short blob) or garbage insertion (long blob).
**Fix:** Replace `debug_assert_eq!` with a hard `if` error return.

#### IDMAP-05 Vbyte carry buffer has no bounds check — stack OOB write (UB) — High
**Lines:** 457–479, 489–510
**Bug:** Carry buffer is `[u8; 16]`. If malformed compressed data has all-continuation-bits set, `carry_len` exceeds 16 and `carry[carry_len]` writes past the stack array. This is undefined behavior — stack corruption.
**Fix:** Add `if carry_len >= carry.len() { return Err(…); }` before writing.

#### IDMAP-06 IndexCache truncates dense index to u32 — Medium
**Line:** 580
**Bug:** `result.map(|i| i as u32)` wraps for maps with ≥ 4B objects. Returns `Some(wrong_index)` instead of correct result.
**Fix:** Use `u64` for cache values, or assert index fits in `u32`.

---

### 13. Compressed Vectors (`src/cvec.rs`)

#### CVEC-01 `CompressedU64::compress` assumes native byte order — `restore` uses little-endian — High
**Lines:** 399–401 (compress), 418–422 (restore)
**Bug:** `from_raw_parts` captures native byte order. `restore` unconditionally uses `from_le_bytes`. On big-endian (s390x, SPARC), this silently byte-swaps all values. No compile-time guard (unlike `zstd_compress_u32_le` which has `compile_error!`).
**Fix:** Add `#[cfg(not(target_endian = "little"))] compile_error!(…)`, or use explicit `to_le_bytes()` in compress (eliminates the `unsafe`).

#### CVEC-02 `stream_u32s` trailing carry bytes silently dropped in release — Medium
**Line:** 346
**Bug:** `debug_assert_eq!(carry_len, 0)` at end of stream. If decompressed length isn't multiple of 4, trailing bytes are dropped silently in release. Returned `Vec<u32>` is 1 element short.
**Fix:** Replace `debug_assert` with `if carry_len != 0 { return Err(…) }`.

#### CVEC-03 `self.len * 4` overflows on 32-bit targets — Medium
**Lines:** 187, 191
**Bug:** `self.len * 4` is `usize` multiplication. On WASM32, arrays > 1 GiB overflow, producing a short `from_raw_parts_mut` slice.
**Fix:** Use `self.len.checked_mul(4).ok_or_else(…)?`.

#### CVEC-05 `CompressedU64::restore` silently truncates on non-multiple-of-8 decompressed length — Medium
**Lines:** 418–422
**Bug:** `chunks_exact(8)` discards the remainder. No length validation. Returned `Vec<u64>` may be shorter than `self.len`.
**Fix:** Validate `bytes.len() == self.len * 8` before chunking.

---

### 14. Self-Update (`src/update.rs`)

#### UPD-01 No download size cap → OOM — High
**Lines:** 372–376
**Bug:** `read_to_end(&mut buf)` with no limit. A gigabyte response silently exhausts RAM.
**Fix:** Wrap reader in `.take(200_000_000)`.

#### UPD-02 No binary integrity/signature verification — High
**Lines:** 286–293
**Bug:** Downloaded archive is extracted and executed with zero cryptographic verification. Smoke test only checks `--version` exit code. MITM or rogue release → arbitrary code execution.
**Fix:** Fetch companion `.sha256` asset and verify hash before extraction.

#### UPD-03 Predictable temp file name → TOCTOU/symlink race — Medium
**Lines:** 296–317
**Bug:** Temp file always at `<exe>.update_tmp`. Attacker can pre-create as symlink targeting arbitrary file.
**Fix:** Use randomized temp name or `tempfile` crate with `O_EXCL`.

#### UPD-04 UTF-8 boundary panic in changelog preview — Medium
**Lines:** 224–225
**Bug:** `&preview[..72]` panics if byte 72 is inside a multi-byte character (emoji, non-ASCII).
**Fix:** Use `preview.chars().take(72).collect::<String>()`.

#### UPD-06 JSON fetch has no size limit — Medium
**Lines:** 380–391
**Bug:** `serde_json::from_reader` on `/releases?per_page=30` — malicious response of 100 MB fully parsed.
**Fix:** Wrap reader in `.take(4_000_000)`.

---

### 15. HPROF Reader (`src/reader.rs`)

#### RDR-02 Unbounded format-string read → OOM on corrupt file — Medium
**Lines:** 281–290
**Bug:** NUL-terminated format string read byte-by-byte with no length limit. Non-HPROF file or corrupt header reads until EOF — potentially gigabytes into memory.
**Fix:** Cap at 256 bytes: `if s.len() >= 256 { return Err(…) }`.

#### RDR-05 `ensure(n)` not guarded against `n > BUF_CAP` — Medium
**Lines:** 327–340
**Bug:** Precondition `n <= BUF_CAP` is not enforced. Future callers with dynamic `n` would silently fail on valid data.
**Fix:** Add `if n > BUF_CAP { return Err(…) }`.

#### RDR-06 `Box::leak` of tar/zip archives — permanent FD + memory leak — Low
**Lines:** 155, 232
**Bug:** Archives leaked for `'static` lifetime. Multi-file processing leaks FDs indefinitely.
**Fix:** Use self-referential struct pattern or eagerly decompress into `Vec<u8>`.

#### RDR-07 Partial record header treated as clean EOF — Low
**Lines:** 440–453
**Bug:** EOF after tag byte but before length field is indistinguishable from normal end-of-file on plain `.hprof`.
**Fix:** Only treat EOF on the very first byte as clean; any subsequent EOF is an error.

---

### 16. Retained Size (`src/retained.rs`)

#### RET-01 `sp_new` u32 truncation → OOM or panic at depth ≥ 2³² — High
**Lines:** 156, 161–163
**Bug:** `(stk.len() + 1) as u32` wraps to 0 at depth ≥ 2³². Then `0u32 - 1` panics in debug or triggers `depth_counts.resize(usize::MAX, 0)` OOM in release.
**Fix:** Track stack depth as `usize`; clamp to `u32::MAX - 1` when storing into `class_to_last_depth`.

#### RET-02 Same wrap silently suppresses `hasSameClassAncestor` bits — Medium
**Lines:** 156, 168–174
**Bug:** Wrapped `sp_new = 0` is the sentinel for "not on stack". All objects below depth 2³² lose their `hasSameClassAncestor` flag → wrong leak-suspect percentages.
**Fix:** Same as RET-01.

---

### 17. Sweep / Comparison (`src/sweep.rs`)

#### SWP-01 `gate_pass()` uses `summary.fail` but render uses fields walk — can diverge — Medium
**Lines:** 93–95, 132–144
**Bug:** Gate verdict and rendered FAIL list are computed from different sources. If the JSON producer's summary disagrees with its fields list, the gate can FAIL while the rendered output shows no failures (or vice versa).
**Fix:** Derive `n_fail` from the fields walk, not from the summary.

---

### 18. Heap Redaction (`src/redact.rs`)

#### RED-01 `write_record_header` silently truncates length > 4 GiB — Medium
**Line:** 439
**Bug:** `(length as u32).to_be_bytes()` without bounds check. Records exceeding 4 GiB are written with a truncated length header, corrupting the output.
**Fix:** Use `u32::try_from(length)?`.

#### RED-02 `copy_exact` / `write_zeroes` truncate `n` on 32-bit targets — Medium
**Lines:** 739, 759
**Bug:** `let mut remaining = n as usize` wraps on 32-bit platforms. Function returns `Ok(())` after writing far fewer bytes than requested.
**Fix:** Use `usize::try_from(n)?`.

#### RED-03 `START_THREAD` thread-name IDs not in `structural_string_ids` — High
**Lines:** 96–158
**Bug:** `START_THREAD` records are never parsed in pass 1 (fall into the `_ => skip` arm). Thread-name strings are zeroed in Complete mode. Intent is undocumented — may be correct, but fragile and non-obvious.
**Fix:** Add explicit match arm with intent comment. Optionally preserve thread names by adding their IDs to `structural_ids`.

#### RED-04 Unknown type code falls back to 1 byte → stream desync, incomplete redaction — High
**Lines:** 768–782
**Bug:** `value_size` / `elem_byte_size` return `1` for unknown type codes. If actual size is 4 or 8 bytes, subsequent fields are misread. All fields after the unknown one are read from wrong offsets → primitive values leak unredacted. Entire heap segment may be copied verbatim via the unknown-tag fallback.
**Fix:** Return `Err(…)` for unknown type codes. Propagate error up through `scan_class_dump` and `redact_class_dump`.

#### RED-06 `unwrap_or(HprofType::Int)` for unknown field type misaligns layout — Medium
**Line:** 338
**Bug:** Unknown instance-field type code treated as 4-byte Int. If actual field is 8 bytes (Long/Double), all subsequent fields are offset by 4 bytes → partial plaintext leak + broken object refs.
**Fix:** Skip the entire class's field list if any field has an unknown type code, triggering the conservative full-zero path in pass 2.

---

## Round 5 — HTTP server, diff reports, MAT export (deeper), dominator, pass1, collections, config

Audited files: `serve.rs`, `query/server.rs`, `diff_reports.rs`, `diff/compare.rs`, `mat/mod.rs`, `mat/int_index_1n.rs`, `mat/serial.rs`, `dominator.rs`, `rpo_dfs.rs`, `pass1.rs`, `collections.rs`, `classify.rs`, `collection_config.rs`. Also ran static analysis across all 85 `.rs` files (754 `unwrap()` calls, 276 `as u32` casts, 19 `debug_assert`s found).

### 14. HTTP Server (`serve.rs`, `query/server.rs`)

#### SRV-05 Header injection via unsanitised filename stem — High
**Lines:** serve.rs 440–450
**Bug:** The dump path's `file_stem()` is interpolated verbatim into a `Content-Disposition` header. A path containing `"` or CRLF injects extra HTTP headers.
**Fix:** Sanitise stem to `[a-zA-Z0-9._-]` before embedding.

#### SRV-06 `unwrap()` on parsed header panics worker thread — High
**Line:** serve.rs 470, query/server.rs 899
**Bug:** `.parse::<tiny_http::Header>().unwrap()` in worker thread. If `ctype` ever contains invalid bytes, the thread panics and is never respawned, permanently shrinking the pool.
**Fix:** Use `Header::from_bytes()` with fallback to `application/octet-stream`.

#### SRV-07 Unbounded body read in `query --server` — Medium
**Line:** query/server.rs 871
**Bug:** `read_to_string(&mut body)` has no size limit. Client can POST gigabytes → OOM. The main `serve.rs` correctly uses `.take(64*1024)` but this path does not.
**Fix:** Apply `.take(MAX_BODY)` before `read_to_string`.

#### SRV-09 Race condition in analysis-start — Medium
**Lines:** query/server.rs 432–435
**Bug:** Non-atomic load+store on `analysis_state`. Two concurrent requests can both start analysis, doubling memory/CPU.
**Fix:** Use `compare_exchange(0, 1, AcqRel, Relaxed)` instead of separate load+store.

#### SRV-13 No query timeout — slow OQL blocks workers forever — Medium
**Lines:** serve.rs 412–474
**Bug:** OQL queries execute synchronously on worker threads with no timeout. A pathological query (e.g., `path()` on entire heap) blocks a worker indefinitely. A small number of concurrent slow queries exhausts the entire pool.
**Fix:** Execute queries on a dedicated bounded pool with a wall-clock timeout.

### 15. Diff Reports (`diff_reports.rs`, `diff/compare.rs`)

#### DIFF-04 `u64 as i64` cast on delta values — Medium
**Lines:** 223, 228, 257, 262, 367, 449, 452
**Bug:** All delta calculations cast `u64` values to `i64` before subtraction. If retained sizes exceed `i64::MAX` (~9.2 EB), the cast silently wraps.
**Fix:** Use `saturating_sub` or checked arithmetic.

#### DIFF-05 `gross_shrink_retained` printed with negative sign alongside "reclaimed" — Medium
**Lines:** 582–587
**Bug:** The verdict says "−340 KB reclaimed" — the negative sign plus the word "reclaimed" is a double-negative. User confusion.
**Fix:** Format reclaimed values as their absolute magnitude.

#### DIFF-08 Duplicate class names in suspect HashMap shadow each other — Medium
**Lines:** diff/compare.rs 409–413
**Bug:** `HashMap::collect()` silently overwrites earlier entries with the same key. Duplicate-classloader suspects lose the first entry's comparison.
**Fix:** Use `HashMap<&str, Vec<&Suspect>>` like the histogram comparator does.

#### DIFF-09 Growth-leaders Δ column contradicts sort key — Medium
**Lines:** 288–305, 674–684
**Bug:** Growth leaders are sorted by `peak_delta` (worst-case spike), but the displayed Δ column shows `delta_retained` (first→last), which can be zero or negative for spiked-then-reclaimed classes. Misleading for users.
**Fix:** Add a `peak_delta` column or annotate the discrepancy.

#### DIFF-12 Wrong percentage denominator in suspect comparison — Medium
**Lines:** diff/compare.rs 415, 448
**Bug:** Percentage uses `leaks.total_shallow` instead of `overview.total_shallow`. These can diverge for truncated/filtered dumps → false FAILs.
**Fix:** Use `ours.overview.total_shallow` as the denominator.

### 16. MAT Export (deeper audit — `mat/mod.rs`, `int_index_1n.rs`, `serial.rs`)

#### MAT-07 Delta overflow truncates header position in IntArray1N — High
**Lines:** int_index_1n.rs 286–289
**Bug:** `(delta + 1) as i32` silently wraps when a single entry has > 2.1B children. Corrupts all subsequent header positions.
**Fix:** Return error if delta exceeds `i32::MAX`.

#### MAT-08 `utf_bytes` truncates names > 65535 bytes — High
**Line:** serial.rs 193
**Bug:** String length cast `s.len() as u16` wraps for long names (deep generics, CGLIB proxies). Stream misaligns permanently.
**Fix:** Assert length ≤ u16::MAX, or use `TC_LONGSTRING` for long names.

#### MAT-09 ClassImpl layers vec inverted — superclass first instead of subclass first — High
**Lines:** mod.rs 818–903
**Bug:** `write_object_keyed` reverses layers, but layers are passed superclass-first. Result: ClassImpl fields and AbstractObjectImpl fields are swapped in the serialisation stream. MAT reads wrong values for every ClassImpl.
**Fix:** Swap the two layer entries so layers[0] = ClassImpl (most-derived).

#### MAT-14 `superClassId` always 0 — Medium
**Line:** mod.rs 810
**Bug:** Hardcoded placeholder. MAT cannot traverse class hierarchy → wrong retained-heap grouping.
**Fix:** Build addr→mat-id lookup and populate from class's super_addr.

#### MAT-16 `emit_dom_in` writes raw dense-IDs, not MAT-IDs — High
**Lines:** mod.rs 323–331
**Bug:** The entire dominator tree in MAT uses internal dense IDs instead of address-sorted MAT IDs. Every parent relationship is wrong.
**Fix:** Translate each `idom[i]` through `mm.translate()` before emitting.

#### MAT-18 `block_data` hard-panics for payloads ≥ 256 bytes — Medium
**Lines:** serial.rs 384–389
**Bug:** `assert!(bytes.len() < 256)` panics instead of using `TC_BLOCKDATALONG` for larger payloads.
**Fix:** Add `TC_BLOCKDATALONG` path for blocks ≥ 256 bytes.

### 17. Dominator Tree (`dominator.rs`, `rpo_dfs.rs`)

#### DOM-01 `vroot = n as u32` wraps for heaps with ≥ 2³² objects — High
**Lines:** dominator.rs 36, rpo_dfs.rs 59; also lines 62–63
**Bug:** Virtual root sentinel, `semi[]`, `label[]` init all truncate. Corrupts every `idom` entry that should reference vroot; also collides with real object indices.
**Fix:** `n.try_into::<u32>().map_err(…)?` at function entry.

#### DOM-03 `compress()` while-guard causes OOB when chain hits virtual root — High
**Line:** dominator.rs 618
**Bug:** `ancestor[ancestor[x]] != UNDEFINED` double-dereferences. When `ancestor[x] == UNDEFINED (u32::MAX)`, the inner indexing reads `ancestor[u32::MAX as usize]` → OOB panic (debug) or UB (release). Fires on every GC-root-adjacent subtree during path compression.
**Fix:** Add `ancestor[x as usize] != UNDEFINED &&` guard before the double-dereference.

### 18. Pass 1 (`pass1.rs`)

#### P1-04 Class-index ≥ 2³⁰ bleeds into kind bits — no guard — High
**Lines:** 578, 609, 640
**Bug:** Class addresses are interned to a `u32` index, but only 30 bits are available (upper 2 bits are kind flags). At ≥ 2³⁰ distinct classes, idx overflows into the kind field → every class ID after that point is wrong. No check, no error.
**Fix:** Assert `class_addr_table.len() < (1u32 << 30)` before each intern.

#### P1-05 `(0..n as u32).collect()` wraps for > 4B records — Medium
**Line:** 355
**Bug:** If pre-dedup record count exceeds `u32::MAX`, the sort-index range wraps, producing a short `order` vec. Tail of all parallel arrays is left unsorted → corrupt `id_map`.
**Fix:** Assert `n <= u32::MAX as usize` before constructing order.

### 19. Collections & Config (`collections.rs`, `classify.rs`, `collection_config.rs`)

#### COLL-01 `ArrayDeque` missing `size_field` — count uses non-null slots instead of `(tail-head)&mask` — Medium
**Lines:** collections.rs 183–185
**Fix:** Add `size: ("size", "java/util/ArrayDeque")`.

#### COLL-02 Guava `ImmutableMap` — abstract base class, `table` field doesn't exist there — Medium
**Lines:** collections.rs 245–247
**Bug:** The descriptor names `ImmutableMap` (abstract), but concrete data lives on `RegularImmutableMap`. Every ImmutableMap in the heap is silently skipped.
**Fix:** Target `RegularImmutableMap` with field `keys`.

#### CFG-01 Config file name mismatch: `.hprof-analyzer.toml` in CWD vs `collections.toml` in HOME — High
**Lines:** collection_config.rs 268, 273
**Bug:** Users following the CWD convention who move their config to `~/.config/hprof-analyzer/.hprof-analyzer.toml` get no config at all. Silent failure, no warning.
**Fix:** Unify the filename or search both names.

#### CFG-03 Single bad TOML entry drops entire user config — Medium
**Lines:** collection_config.rs 255–261
**Bug:** One invalid `[[collection]]` entry causes `parse_toml_str` to fail, and the `unwrap_or_default()` fallback drops all valid entries too.
**Fix:** Validate per-entry, keeping valid ones and warning about bad ones.

#### CLS-01 `lint_category_order` false positive on non-boundary prefix matches — Medium
**Lines:** classify.rs 156–159
**Bug:** `"com.foobar".starts_with("com.foo")` fires lint, but these are independent packages. The lint should check at dot boundaries.
**Fix:** Require `ep.ends_with('.') || later_prefix[ep.len()..].starts_with('.')`.

#### CLS-02 `ThreadPoolExecutor` in framework list has no trailing dot — Medium
**Line:** classify.rs 109
**Bug:** `ScheduledThreadPoolExecutor` and `$Worker` inner class are unintentionally swept into `framework` category.
**Fix:** Add trailing dot, or use an exact-match variant.

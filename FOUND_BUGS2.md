# FOUND_BUGS2.md — hprof-analyzer bug ledger (post-verification)

Status after the 2026-11-17 verification sweep (CLI reproducer matrix + MCP stdio
matrix vs `target/release/hprof-analyzer`, fresh binary). **All entries below are
verified-still-OPEN**; everything else was verified fixed and removed. Fixed and
verified this session: A1, B1, B2, B3, B4, B5, B7, C1, C2, C3, C4(fixed parts),
C6(fixed parts), D1, D2, D3, F1, G2, H1, H2, H4, H5, H6, H7, I6(positive).

Legend: OPEN / DOC-GAP. Every bug has a reproducer. Cache note per CLAUDE.md:
after code changes, `heap cache-clear <dump>` or delete `<dump>.hprof-cache/`.

---

## B. OQL engine / query tool

### B6. OPEN (rest) — documented features that don't work (agent E)
- FIXED: `WITH` CTEs are now IMPLEMENTED (token-level pre-parser + desugar into
  the FROM-subquery machinery; single + multiple CTEs, aliases, UNION branches;
  chained/recursive CTEs rejected with clear messages; docs + oql_docs updated;
  parse + execution regression tests). Two regressions found & fixed on the way:
  (a) the scan executor's `class_matches` guard was dropped by an unrelated
  FromSource refactor — every executor matched every object (a Thread-only
  inner subquery returned all 203,782 reachable objects); (b)
  `run_oql_escalated` had no subquery routing — the B1 fix only patched the MCP
  tool, so `heap query` panicked ("flat results shorter than groups describe");
  routing now happens once inside run_oql_escalated for every caller.
- Virtual tables `__classes__`/`__gc_roots__`/`__threads__`: implementation
  landed in the working tree (pass2 materialization + VTableData) — verify:
  `heap query <dump> --oql 'SELECT COUNT(*) FROM __threads__'`.
- N of the examples inside `get_oql_docs` fail as-is. Repro: run each doc
  example verbatim via `heap query tests/fixtures/dump_4_philosophers.hprof --oql '...'`.

### B8. OPEN — `INSTANCEOF` on an interface silently returns 0 rows
- Repro: `SELECT COUNT(*) FROM INSTANCEOF java.util.List` → 0 even though
  ArrayList/HashMap$Node implement it. Either expand interfaces via the
  subclass table or warn that only class hierarchies are walked.

### B9. OPEN — FROM-subquery silently ignores outer LIMIT (agent E)
- `SELECT ... FROM (SELECT ...) t LIMIT 10` returns the full inner result until
  the oversized-result guard fires; LIMIT is silently dropped.
- Repro: `heap query tests/fixtures/dump_4_philosophers.hprof --oql 'SELECT
  @objectId FROM (SELECT @objectId FROM java.lang.Thread) t LIMIT 3'` → 27 rows.

## C. MCP server behavior

### C4. OPEN (rest) — alloc_sites lies (== H3 first bullet)
- `alloc_sites`: object_count 239,355 > GC-reachable (203,782) and file total
  (236,457); retained_total 37.7 MB > 12 MB heap; shallow_total 12,986,608 B ≠
  OQL SUM(@usedHeapSize) 4.3 MB; `frames: []` while `traces_present: true`;
  `limit` ignored (1 site returned).
- Repro: MCP `get_report({section:"alloc_sites"})` on leak-clinic.

### C5. OPEN — silent empties / wrong values in report sections and views
- `field_stats` → `null` (always?); `thread_locals` → `[]` without
  `--find-duplicates`, unexplained; `arrays` superseded by `collections` but
  still offered.
- `classes-per-loader` view returns 1 for every loader (counts loaders, not
  classes). (Thread views' `@name` was fixed — re-verify the 4 views.)
- Views `static-collections`, `unclosed-resources` silently empty without
  `collections: true` at load; `list_views` documents them as `needs_retained`
  only. Repro: `hprof-analyzer heap views`, then run each view OQL via `heap query`.

### C6. OPEN (rest) — graph-tool nits (agent H)
- `get_report(leaks)` `path` always empty; `accumulation_*` always null.
- `find_incoming_references` can return duplicate edges for the same source object.
- `get_retained_set.class_filter` filters the object list but not `class_summary`.
- `total_shallow_in_subtree` excludes the root (diverges from root_retained_bytes).
- GC-root terminal nodes carry synthetic out-of-range indices (623284 > 562,385
  objects on gauss-mix) — undocumented.
- Repro: load gauss-mix with with_graph:true, walk the top suspect.

## D. Robustness / parsing

### D4. OPEN — index-space confusion documented? (verify + complete)
- Banner/inspect range errors now mention "covers objects AND classes/other
  entries" — verify the load banner itself also states both numbers
  (`Objects: 437704` vs range `0..482360` on clinic).

## E. Redaction / secrets

## G. Tooling/config environment

### G1. OPEN — repo `.mcp.json` points at `target/release/hprof-analyzer`
- Fresh checkout → ENOENT on `claude mcp list` until a release build exists.

## H. Wave-5 findings

### H3. OPEN — `get_report` truth bugs (report-N / report-1.md)
- `field_stats` → `null`; `thread_locals` → `[]` (2 ThreadLocal instances exist);
  `framework` → `[]`; `waste.waste_breakdown` → always `[]`;
  `waste.reachable_bytes` vs `total_bytes` relation unexplained.
- `limit` parameter has no effect on `waste`, `retainers`, `arrays`,
  `collections`, `indicators`, `alloc_sites`.
- `indicators.anonymous_class_count: 204` counts distinct `$$` class names (163),
  not instances (1,140+).
- Repro: `get_report({section:"alloc_sites"})` + clinic dump.

## I. Wave-6 findings

### I1. OPEN — load banner "Top Leak Suspects" misleads (agent T, F1)
- Banner list comes from histogram top-by-retained, not the leak-suspect
  detector (dump_1_mnemonics: banner names Thread/HashMap; real suspects are
  `HashMap$Node[]`/`Object[]`). Repro: `heap load tests/fixtures/dump_1_mnemonics.hprof`
  vs `heap report <dump> --section leaks`.

### I2. DOC-GAP — `SUM(@retainedHeapSize)` double-counts shared dominatees (agent T)
- OQL SUM counts shared children once per holder; histogram retained uses
  single-ownership dominator accounting. Document in OQL docs + query result hint.

### I3. OPEN — overview/leaks sections exceed usable MCP size on an 11 MB dump
- `get_report("overview")` = 763,833 chars; the 256 KB cap (C3) stubs them.
  A condensed/paginated mode for these two core sections would serve agents
  better than a stub.

### I4. DOC-GAP — DirectByteBuffer off-heap total not verifiable without graph
- The triage's native figure sums `capacity` fields; document the requirement
  next to the signal.

### I5. OPEN — empty-collection signal overstates on benchmark dumps (agent T)
- 72–83% of "empty" collections are JDK/Scala singleton sentinels (Nil,
  Collections.emptyList()). Exclude known sentinels or soften the wording.

### I7. OPEN — views requiring `collections` need list_views flags (from J)
- `static-collections`/`unclosed-resources` need an undocumented load flag;
  either degrade gracefully with a hint or mark them in `list_views`.

## Verified-fixed summary (details removed per cleanup)

A1 bare-star FROM · B1 MCP subquery routing · B2 unknown-FROM error · B3
literal-list IN (false-positive repro; regression tests added) · B4 AS-retained
error text · B5 ORDER BY note docs · B7 replace() + window columns on scan paths
· C1 FIFO ordering gate · C2 0-based index twins · C3 output caps + default
triage · C4 HeadlineRetainer + Component basis · C6 browse_dominators caps ·
D1 truncated-dump refusal (--force) · D2 friendly input errors + MCP path hints ·
D3 nonneg index params · F1/H6(f) cache-clear all entries · G2 all 18 tools ·
H1 session epoch + expected_epoch enforcement · H2 graph reload cycle recovery ·
H4 gc_roots/field_name · H5 validation footguns · H6 CLI nits · H7 @name +
LIMIT footer · I6 redaction pipeline (positive).

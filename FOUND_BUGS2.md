# FOUND_BUGS2.md — hprof-analyzer bug ledger (post-campaign)

Final status after the 2026-11-17 campaign (CLI reproducer matrix + MCP stdio
matrix + cheap-agent implementation batch, all verified against fresh builds).
**This file now records only what is genuinely still open.** Everything else
was fixed and verified; see the summary at the bottom.

Legend: OPEN / DOC-GAP / ENHANCEMENT. Cache note per CLAUDE.md: after code
changes, `heap cache-clear <dump>` or delete `<dump>.hprof-cache/`.

---

## Still open

Nothing from the campaign's original list. The I3 enhancement and the B6
sweep upkeep item were closed after the campaign (see below); new bugs go in
fresh entries.

---

## Adjudications & notes worth keeping

### E1. ADJUDICATED (DIVERGENT — warning stands)
- Empirical re-verification (fresh caches, philosophers fixture): complete-mode
  redaction still changes structure — original top suspects `InTxnImpl`
  (2.7 MB) and `LazyVals$` (2.5 MB) VANISH in the complete-redacted dump;
  `Object[]`, `PhilosopherThread`, `JarFile`, `Class` appear instead; 1,395
  objects slip out of the reachable graph (reachable heap 11.6 → 9.0 MB).
  Lean mode produces a byte-identical leaks section. The "complete-mode alters
  dominator/retained structure" warning (CLI `redact --complete` + MCP redact
  tool description + response note) is accurate and must stay; commit-message
  claims of complete-mode structural parity are empirically false. Unknown-class
  instance preservation (2277f297) is real but does not restore parity.

### I5 (verified). Empty-collection sentinels
- The campaign premise overstated: sentinels are per-JVM singletons (≈1 per
  dump), not 72–83% of empties. The signal now excludes and labels them
  ("N additional empties are recognized JDK/Scala singleton sentinels …
  excluded from this signal"), `singleton_sentinels` counted separately;
  wording softened. Verified on mnemonics/scala-doku/philosophers with fresh
  caches; golden fixtures re-baselined.

### B8 (resolved by docs)
- `FROM INSTANCEOF <interface>` returning 0 rows is a data limitation, not a
  bug: HPROF CLASS_DUMP records carry no implemented-interface metadata, so
  interface assignability cannot be determined statically. docs/OQL.md states
  this with the AbstractMap workaround. No engine change planned.

---

## Fixed & verified this campaign (summary; details in git history)

- **A1** bare-star FROM · **B1** subquery routing everywhere (incl. the
  `run_oql_escalated` library half found during CTE work) · **B2** unknown-FROM
  error (MCP isError) · **B3** literal-list IN (false-positive repro; tests) ·
  **B4** AS-retained error · **B5** ORDER BY unselected-key sort + footer ·
  **B7** `replace()` + window columns on all scan paths · **B9** FROM-subquery
  outer LIMIT (already fixed at HEAD — ledger was stale; regression test added)
- **B6a** virtual tables `__classes__`/`__gc_roots__`/`__threads__`
  materialized in pass2 · **B6b** `WITH` CTEs implemented (desugar into
  FROM-subqueries; chained/recursive rejected honestly; two regressions fixed
  on the way: scan-executor `class_matches` guard dropped by an unrelated
  refactor, and missing subquery routing in `run_oql_escalated`)
- **C1** FIFO ordering gate · **C2** 0-based index twins · **C3** output caps +
  default triage · **C4/H3** field_stats dispatch, limits honored for
  waste/retainers/arrays/collections/indicators/alloc_sites, honest
  alloc_sites/_note semantics, `anonymous_class_instances`, empty-section
  notes · **C5** classes-per-loader view made honest (OQL cannot express
  per-loader class counts — loader id lives only in CLASS_DUMP headers; view
  renamed/relabelled, report `classloaders` section referenced),
  `@name` thread views, field_stats/thread_locals notes · **C6** all graph-tool
  nits (dedupe + edge_count, class_summary filter consistency, root shallow
  inclusion, synthetic-index marking) · **I7** `needs_collections` flags in
  list_views/heap views for `static-collections`/`unclosed-resources`
- **D1** truncated-dump refusal + `--force` · **D2** friendly input errors +
  MCP path hints · **D3** non-negative index params · **D4** index-space
  banner note
- **E1** lean-mode byte-identical leaks (verified) · complete-mode warning kept
- **F1/H6** cache-clear clears ALL entries (+ dir), cache-list shows every
  entry, `--category` ignored-flags warnings, browse/inspect/histogram nits
  (range errors, `object_index` + alias, Class fields note, `--class-filter`,
  MiB labels)
- **G1** mcp-launcher (PATH → target/release → actionable error) · **G2** all
  18 tools
- **H1** session epoch + `expected_epoch` enforcement · **H2** graph reload
  cycle recovery · **H4** get_gc_roots/get_field_name live · **H5** validation
  footguns (empty oql, bogus docs topic, histogram cap, error-shape)
- **I1** load banner leak suspects from the detector (honest relabelling of
  histogram tops) · **I2/I4** docs: SUM(@retainedHeapSize) double-counting
  caution, DirectByteBuffer off-heap with-graph requirement · **I5** (see
  above) · **I6** redaction pipeline verified on clinic
- **I3** (closed post-campaign) condensed mode for `overview`/`leaks`:
  `cap_obj_arrays` truncates every list to `limit` (overview default 12/max
  200, leaks default 10/max 50), records `<field>_total` per truncated list,
  `_arrays_capped` + `_note` on the section — verified on leak-clinic:
  overview 764 k → 189 k chars of REAL content (no stub), leaks 60 k → 30 k,
  `limit` widens/narrows on demand
- **B6 (rest)** (closed post-campaign) `get_oql_docs` example sweep runs green
  (`tests/oql_docs_sweep.rs`, part of `cargo test`): the extractor treats
  WITH-blocks as single statements, and every documented example now parses
  and executes, including the WITH CTE example (identity-carrying CTE bodies)

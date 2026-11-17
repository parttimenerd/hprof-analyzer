# FOUND_BUGS2.md — evening/night campaign vs hprof-analyzer 0.3.4

Night status: fixes are being implemented and committed per logical chunk;
reproducers below double as the post-fix verification list (run them after
`cargo build --release`). Wave-5/6 subagent reports are in
`/var/folders/4c/yz9tjkbx3k980r1ftjqp_xrh0000gn/T/opencode/agent-test/report-*.md`
(+ `report-1.md` = the truth-audit, agent N wrote a different filename);
fix proposals in `.../agent-test/fixes/*.md`.

Campaign: multiple subagents driving the MCP server (`target/release/hprof-analyzer mcp`)
and the `heap` CLI subcommands against `tests/fixtures/*.hprof` and
`demo/spring-leak-clinic/leak-clinic.hprof`. Agent reports live in
`/var/folders/4c/yz9tjkbx3k980r1ftjqp_xrh0000gn/T/opencode/agent-test/report-*.md`.

Status legend: OPEN / FIXED / DOC-GAP. Every bug has a reproducer.
Cache note per CLAUDE.md: when testing after code changes, `heap cache-clear <dump>` or
delete `<dump>.hprof-cache/` — the cached report is NOT re-evaluated otherwise.

---

## A. Fixed during the campaign

### A1. FIXED — `FROM Xyz*` (bare trailing star) returned 0 rows  [committed]
- Was: parses as one ident token (`parse.rs:149` includes `*` in ident chars) but
  `class_name_matches` (`src/query/execute.rs`) only recognized `.*`/`/*` suffixes → fell
  through to exact match → silent 0 rows. Doc example `FROM com.example.MyCache*`
  (`src/mcp/oql_docs.rs:409`) taught the broken form.
- Repro (was): `hprof-analyzer heap query tests/fixtures/dump_4_philosophers.hprof --oql
  'SELECT COUNT(*) FROM java.util.zip.ZipFile*'` → 0 (regex form: 108; exact `$Source`: 19).
- Fix: bare-trailing-`*` branch in `class_name_matches` (prefix match incl. `$` inner
  classes); docs + doc example updated; unit tests `bare_trailing_star_is_prefix_glob`,
  `pkg_dot_star_still_requires_separator_boundary`. Now → 59.

## B. OQL engine / query tool

### B1. FIXED (critical) — MCP `query` crashes the whole server on subqueries
- (a) `IN (SELECT ...)`: MCP `query` always runs `run_oql_escalated` which hardcodes empty
  membership sets → hits `unreachable!` at `src/query/execute.rs:2950` → panic → with
  `panic=abort` (Cargo.toml:127) the process dies; MCP client must reconnect + reload.
- (b) Aggregates over FROM-subquery (`COUNT(*) FROM (SELECT …) t`): `.expect()` fails at
  `src/query/run.rs:3509` (CountFromSubquery slots get no executor in pass2 → results
  shorter than groups). CLI routes these to the two-phase pipeline (`main.rs:3925-3956`);
  the MCP path (`mcp/mod.rs:1585-1625`) doesn't.
- Repro: via MCP `query` tool:
  `SELECT s.@displayName FROM java.lang.String s WHERE s.@objectId IN (SELECT x.@objectId FROM java.lang.Thread x)`
  `SELECT COUNT(*) FROM (SELECT @objectId FROM java.lang.Thread) t`
- Fix: MCP query now routes plans containing
  `from_subplan`/`in_subplans`/`exists_subplans` to `query::run::run_single_dump`, matching
  CLI two-phase execution for subqueries.

### B2. FIXED — unknown class in FROM returns 0 rows silently
- Repro: MCP `query` `{oql: "SELECT * FROM com.does.not.Exist"}` → `{"columns":["*"],"row_count":0}`.
- Fix: MCP query now returns explicit error text for missing exact FROM classes:
  `class <name> not present in dump`.

### B3. DOC-GAP — `IN` with a literal list was documented as subquery-only
- Engine supports literal-list `IN` / `NOT IN`; docs now describe both forms.
- Repro: `heap query tests/fixtures/dump_4_philosophers.hprof --oql 'SELECT @objectId FROM
  java.util.zip.ZipFile$Source WHERE @objectId IN (1,2,3)'` → 0 rows, no error.

### B4. FIXED — `AS retained` alias collides with `AS RETAINED SET`
- Repro: `heap query tests/fixtures/dump_4_philosophers.hprof --oql 'SELECT COUNT(*) AS
  retained FROM java.util.zip.ZipFile*'` → `error: expected SET after 'AS RETAINED'`.
- Fix: parser error now explicitly says `RETAINED` is reserved in this position and
  suggests an alias such as `retained_count`.

### B5. DOC-GAP — `ORDER BY` unselected key note promise was stale
- Engine sorts by unprojected keys internally; the previous docs promise of a
  "sort not applied" note was outdated and removed.

### B6. OPEN (docs) — documented features that don't work (agent E)
- FIXED (docs): `VALUES` inline tables and `JOIN VALUES` examples now use OQL-valid
  double-quoted strings in docs (`docs/OQL.md`). Added MCP regression tests for
  `FROM (VALUES ...)` and `JOIN (VALUES ...)` execution.
- Virtual tables `__classes__`/`__gc_roots__`/`__threads__`: silently 0 rows (docs/OQL.md:102-104).
- `WITH` CTEs: rejected; error redirects to the FROM-subquery form that crashes (B1b).
- 9 of the examples inside `get_oql_docs` fail as-is.
- Repro: run each doc example verbatim.

### B7. OPEN — window/string functions return null (agent E)
- `ROW_NUMBER()`/`RANK()`/`DENSE_RANK()`, `substring()`, `replace()` parse then return
  `null` for every row. Should be rejected at parse time if unsupported.

### B8. OPEN — `INSTANCEOF` on an interface silently returns 0 rows
- Repro: `SELECT COUNT(*) FROM INSTANCEOF java.util.List` → 0 even though
  ArrayList/HashMap$Node etc. implement it. Either support interfaces via subclass table or
  warn that only class hierarchies are walked.

### B9. OPEN — FROM-subquery silently ignores outer LIMIT (agent E)
- `SELECT ... FROM (SELECT ...) t LIMIT 10` returns the full inner result until an
  oversized-result guard fires; LIMIT is silently dropped.

## C. MCP server behavior

### C1. FIXED — out-of-order responses under pipelining (state race)  [implemented, pending rebuild]
- Repro (0.3.4 confirmed): one stdin write with load_dump (id=2) + get_session_info (id=3):
  id=3 answered first with `"loaded": false` while id=2's success arrives after.
- Root: rmcp spawns a task per request (service.rs:1580, spawn at 1309-1314); crate-level
  `Arc<Mutex<Option<CachedSession>>>` (mcp/mod.rs:378) protects state, not ordering.
- Fix: FIFO `tokio::sync::Mutex<()>` gate field on the server, acquired as first statement
  of 16 stateful handlers (incl. load_dump's whole spawn_blocking region); read-only static
  tools (list_views, get_oql_docs) stay ungated. Repro above must answer id=3 after id=2.

### C2. FIXED — 0-based twin `object_index` fields  [implemented, VERSION bumped 17→18, pending rebuild]
- Repro (gauss-mix): `get_report(top-objects)` gives `obj_index_1based: 238964` for
  `java.util.zip.ZipFile$Source`; `inspect_object(238964)` returns `ZipFile$Source$Key`
  (wrong object, silently); the correct index is 238963 (`@objectId` agrees).
- Tool descriptions carry a ⚠ note but only mention `dominator_tree + root_path`;
  `top-objects`, `triage.biggest_objects`, `DominatedRow`, `TopArrayRow`,
  `accumulation_obj_1based` are 1-based too and not covered (subagent-verified list in
  handoff section).
- Fix: every producer (report/build.rs ×13 + pass2/fielddecode.rs) now emits a sibling
  0-based `object_index` (model structs updated, serde default for back-compat; schema +
  golden snapshot regenerated). MCP ⚠ note widening to point at the twin still TODO.

### C3. FIXED — `get_report` output bombs  [implemented, pending rebuild]
- `section=all` → 3.8–7.9 MB; `overview` → 1–3 MB; `dominators` → ~600 KB–3.5 MB inline;
  `top` duplicates top-objects/top-classes with zero unique content. No MCP-level output
  cap anywhere (mcp/mod.rs:620-634 sends full JSON).
- Fix: dominators.pairs capped at the serving boundary surfacing
  pairs_total_count/pairs_truncated; all sections now go through heap_ops +
  `serialize_capped` (>256 KB → `{"_truncated":true,"_hint":…,"_bytes":N}`); get_histogram
  limit server-capped; default section changed "all" → "triage".

### C4. PARTIALLY FIXED — triage signals that lie or contradict  [implemented, pending rebuild]
- FIXED: HeadlineRetainer fallback (suspect below the 50% floor) is now Warning with
  "below the N% dominance floor" wording (keeps the value-type hint), so it no longer
  contradicts Concentration's diffuse; tests updated.
- FIXED: Component Retention Imbalance now reads the mutually-exclusive `pct_of_retained`
  basis — no more "retains 439.7% of heap".
- Still OPEN: alloc_sites wrong numbers + traces_present lie + ignored limit (H3).
- `alloc_sites` reports ~91 GB retained on a 12 MB heap and claims `traces_present: true`
  with empty frames (agent I).
- `Component <boot> retains 439.7% of heap (180.5 MB)` — summed per-class histogram
  retained (overlapping retained sets) divided by total heap; renderer even says
  "may exceed 100%". Triage rule should use the mutually exclusive
  `pct_of_retained` basis (build.rs:2004-2013) instead of `top.pct` (triage.rs:3777).
- Repro: `heap report demo/spring-leak-clinic/leak-clinic.hprof --section triage`.

### C5. OPEN — silent empties / wrong values in report sections and views (agents I, J)
- `field_stats` → `null` (always?); `thread_locals` → `[]` without `--find-duplicates`,
  unexplained; `arrays` superseded by `collections` but still offered.
- 4 thread views: `@name` yields `java.lang.Thread` (class name) instead of the thread-name
  string → thread identification impossible via those views.
- `classes-per-loader` view returns 1 for every loader (counts loaders, not classes).
- Views `static-collections`, `unclosed-resources` silently empty because they need
  `collections: true` at load time; `list_views` documents them as `needs_retained: true`
  only.
- Repro: `hprof-analyzer heap views`, then run each view OQL via `heap query`.

### C6. PARTIALLY FIXED — graph-tool truncation/units nits (agent H)
- FIXED: browse_dominators emits `total_children` + `children_truncated` before truncating
  and the `.min(50)` cap is documented in the tool description.
- Still OPEN: leaks `path` always empty; find_incoming_references duplicate edges;
  retained_set class_filter vs class_summary; subtree shallow excludes root; GC-root
  synthetic out-of-range indices undocumented.
- `get_report(leaks)` `path` always empty; `accumulation_*` always null.
- `find_incoming_references` can return duplicate edges for the same source object.
- `get_retained_set.class_filter` filters the object list but not `class_summary`.
- `total_shallow_in_subtree` excludes the root (diverges from `root_retained_bytes`).
- GC-root terminal nodes carry synthetic out-of-range indices (623284 > 562,385 objects on
  gauss-mix) — undocumented.
- Repro: load gauss-mix with with_graph:true, walk the top suspect per agent-H steps.

## D. Robustness / parsing

### D1. OPEN (critical) — truncated dump silently accepted, exit 0
- Repro: `head -c 1000000 tests/fixtures/dump_4_philosophers.hprof > /tmp/trunc.hprof &&
  ./target/release/hprof-analyzer heap summary /tmp/trunc.hprof` → prints suspects
  (0 B retained), exit 0, no truncation warning. CLI and MCP both.
- Root: pass1.rs:328-334 swallows `UnexpectedEof` mid-record (inherited from the
  intentionally-lenient gzip path); `truncated_input` is computed (pass1.rs:341) but only
  rendered as one markdown note; `heap summary` (main.rs:1651) and MCP `load_dump`
  (mcp/mod.rs:511) never check it.

### D2. OPEN — CLI vs MCP error asymmetry for bad inputs (agent G)
- `.lz4` given to CLI → `unsupported id_size in HPROF header: 15733066` (leaks an internal
  field); MCP rejects with an extension check. Empty file → `failed to fill whole buffer`;
  text-as-.hprof → `unexpected eof`. Relative path in MCP fails with no "use absolute path"
  hint (server cwd ≠ caller cwd).

### D3. OPEN — negative `object_index` → raw serde error
- Repro: MCP `inspect_object({object_index: -1})` → `failed to deserialize parameters:
  invalid value: integer \`-1\`, expected u64` (isError=true). Range error (999999999) is
  fine.

### D4. OPEN — index-space confusion: `0..482360` range vs `Objects: 437704`
- Repro: load clinic via MCP; `inspect_object(999999999)` says `0..482360` while the load
  banner says `Objects: 437704`. The extra ~45k are classes/other entries; undocumented.

## E. Redaction / secrets

### E1. OPEN — complete-mode redaction changes the leak-suspects list (agent F)
- Repro: redact philosophers with `--complete`; original top-2 suspects
  (`InTxnImpl`, `LazyVals$`) disappear; `PhilosopherThread`/`JarFile` appear instead →
  a dump shared for leak diagnosis points analysts at different suspects.
- Also: complete mode reports 1,395 fewer objects / 3 MB less heap than the original
  (unexplained); redacted triage gains a `gc-waste: 27.6% unreachable` artifact; lean mode
  keeps `String.hash` non-zero (documented generically, not called out as the example).

## F. Cache behavior

### F1. OPEN — cache-clear only clears the options-hash-matching entry (agent L)
- Sibling entries (plain vs with-graph, other option sets) survive
  `heap cache-clear <dump>`; also `cache-list` shows `0.0 MB` for an eagerly-created empty
  hash dir, and a SIGKILL mid-write can leave a partial file set with no sentinel (silent
  rebuild on next load — correct, just opaque).

## G. Tooling/config environment

### G1. OPEN — repo `.mcp.json` points at `target/release/hprof-analyzer`
- Fresh checkout → ENOENT on `claude mcp list` until a release build exists; plus a
  duplicate working `hprof` server only exists project-scoped in `~/.claude.json`.

### G2. FIXED locally — stale binary / missing graph tools
- 0.3.4 build exposes all 18 tools incl. `get_dominator_path`, `get_retained_set`,
  `find_incoming_references(_by_class)`, `get_outbound_references`, `get_gc_roots`,
  `compare_dumps`, `get_field_name`.

## H. Wave-5 findings (session matrix, truth audit, CLI deep-dive, validation sweep)

### H1. OPEN (critical) — stale indices silently resolve against a newly loaded dump
- After `load_dump(A)` → `load_dump(B)`, `inspect_object`/`get_dominator_path`/
  `browse_dominators` called with indices recorded from A return a **different object from B**
  with no error or warning (report-M cross-contamination table: index 39275 = JarFile 598KB in
  A → Object 16B in B → Class$AnnotationData 24B in C).
- Repro: MCP session: `load_dump(A, with_graph:true)`; note an index from a query; `load_dump(B)`;
  `inspect_object(<same index>)` → success, wrong object.
- Suggested: session epoch stamped into every response; handlers reject/flag out-of-epoch indices.

### H2. OPEN (high) — graph state breaks after A → B → reload A (with_graph:true)
- After that cycle, `find_incoming_references` fails with a bare internal error; in-process
  graph state and on-disk `graph.bin` fall out of sync and the reload path doesn't detect it
  (report-M Finding 7). No stale bleed-through when reloading WITHOUT graph (that path is correct).
- Repro: `load_dump(A, with_graph:true)` → `load_dump(B)` → `load_dump(A, with_graph:true)` →
  `find_incoming_references({object_index: <any>})`.

### H3. OPEN — `get_report` truth bugs (report-N / report-1.md)
- `alloc_sites`: `object_count` 239,355 exceeds both GC-reachable (203,782) and file total
  (236,457); `retained_total` 37.7 MB > 12 MB heap (OQL SUM = 23.9 MB); `shallow_total`
  12,986,608 B ≠ OQL SUM(@usedHeapSize) 4.3 MB; `frames: []` always empty while
  `traces_present: true`; `limit` param ignored; only 1 site returned.
- `field_stats` → `null`; `thread_locals` → `[]` (2 ThreadLocal instances exist);
  `framework` → `[]`; `waste.waste_breakdown` → always `[]`; `waste.reachable_bytes` vs
  `total_bytes` relation unexplained.
- `limit` parameter has no effect on `waste`, `retainers`, `arrays`, `collections`,
  `indicators`, `alloc_sites`.
- `indicators.anonymous_class_count: 204` counts distinct `$$` class names (163), not
  instances (1,140+).
- Repro: `get_report({section:"alloc_sites"})` + `heap query <dump> --oql 'SELECT
  SUM(@retainedHeapSize) FROM INSTANCEOF java.lang.Object'`.

### H4. FIXED — two graph tools were dead (report-P)  [implemented, pending rebuild]
- `get_gc_roots` always returns `{roots: [], shown: 0, total_gc_roots: 1681, truncated: false}`
  — never populated, truncation flag lies (all limit values).
- `get_field_name` always errors `field name pool not available — re-analyze with heap load
  --with-graph` even though the dump WAS loaded via MCP `load_dump({with_graph: true})`
  (graph_loaded:true) — the MCP load path doesn't build the field-name pool; error text also
  points at the CLI command instead of the MCP equivalent.
- Fix: get_gc_roots now enumerates roots from the idom CSR (undef/self-loop/vroot encodings
  accepted) with honest `truncated` flag; MCP load_dump with_graph:true now builds the
  field-name pool (obj_graph switch); get_field_name error text now references
  load_dump({with_graph:true}).
- Repro to re-verify: load philosophers `with_graph:true` → `get_gc_roots({})` returns
  populated roots or truncated:true; `get_field_name(...)` works after MCP graph load.

### H5. OPEN — validation/footguns (report-P)
- FIXED: `get_report({})` now defaults to `triage` (was `all` = 3.8 MB inline).
- `get_histogram({limit: 4294967295})` uncapped (58 KB payload, no server-side guard).
- `query` with empty `oql`: error returned inside a success body.
- `get_oql_docs({topic: "bogus"})` silently returns the full "all" docs — no validation.
- `get_retained_set`/`browse_dominators` with huge `depth`/`width` values: see H6 markers.

### H6. OPEN — CLI deep-dive nits (report-O)
- `heap browse --index <out-of-range>` silently renders `<unknown>` with 0 retained — no error.
- `browse` JSON uses key `index`, `inspect` JSON uses `object_index`.
- `heap inspect` on `java.lang.Class`: no `Fields:` section and no explanatory note; reference
  fields show raw hex address instead of resolved class+index.
- `heap histogram` has no `--class-filter`; two close values round to the same `MB` label;
  unit label says MB but computes MiB.
- `heap cache-list` only shows the non-graph hash entry — graph cache invisible; `--category`
  is silently ignored when a cache exists AND is not part of the cache key → stale
  categorization can persist across option changes.
- `heap cache-clear` does NOT clear the graph cache hash dir → orphaned graph data accumulates
  (matches F1/L).
- Repro: `heap cache-clear <dump>` then `heap cache-list <dump>` (graph entry survives).

### H7. OPEN — thread views report class name instead of thread name (verified directly)
- Repro: `heap query tests/fixtures/dump_4_philosophers.hprof --oql 'SELECT @objectId AS idx,
  @name AS name FROM java.lang.Thread LIMIT 5'` → `name` = `java.lang.Thread` for every row;
  the threads REPORT section has the real names (main, Reference Handler, …), so the OQL
  `@name` attribute doesn't resolve `Thread.name`.
- Also: LIMIT 5 footer prints `(truncated — 5 rows total)` — ambiguous/wrong wording for a
  user-requested LIMIT.

---

## Handoff notes (code-localized root causes)

- Ordering fix: FIFO `tokio::sync::Mutex<()>` tool-gate in `HprofMcpServer`
  (mcp/mod.rs:377-386), acquire in every stateful `#[tool]` handler; rmcp spawns per-request
  tasks (rmcp service.rs:1580/1309-1314).
- OQL crash routing: mirror `main.rs:3925-3956` in `mcp/mod.rs:1601-1622`; related panics:
  `run.rs:3586,3607`, `execute.rs:1230,3609,4230`.
- Truncation: pass1.rs:328-334 swallow point; same pattern in pass2/model.rs (8 sites),
  cvec.rs:385, main.rs:4286-4292; redact relies on lenient parsing
  (redact.rs:1680 test) — keep that path lenient or gate the fix.
- browse_dominators marker: mcp/mod.rs:1544-1571 (`ch.truncate(width)`, cap at
  mcp/mod.rs:740); sibling tools' marker pattern: heap_ops.rs:561-566, 633-636, 781-783,
  961-968; report-side pattern: mcp/mod.rs:1746-1755.
- get_report caps: heap_ops.rs:193 (`"dominators" => to_value(...)` uncapped;
  `IMDOM_PAIRS_CAP = 20_000` at build.rs:86); leaks/threads already post-process at
  heap_ops.rs:141, 147-180.
- 1-based fields: produced at report/build.rs:1004,1020,1282-1294,1662,2120,2552,3279,3807,
  4276,4308,4333,4486,5839 + pass2/fielddecode.rs:459; ⚠ notes at mcp/mod.rs:588,727,752,
  1434 and oql_docs.rs:256,590,662,850-856,942-945,972 (incomplete coverage).
- Triage: HeadlineRetainer triage.rs:669-741 (fallback 695-716), Concentration
  triage.rs:750-825 (diffuse 808-815), culprit floor `CULPRIT_DOMINANT_PCT = 50.0`
  build.rs:4557.
- Components: build.rs:1960-2033 (acc.retained 1982, pct 1999-2003), triage.rs:3769-3803,
  threshold triage.rs:227; sane basis exists (`pct_of_retained` build.rs:2004-2013).
- Remember CLAUDE.md: any change to what `build_model`/`evaluate_triage` emit → bump
  `VERSION` in `src/cache.rs` or test against stale caches silently lies.

## I. Wave-6 findings (fresh-dump sweep T, cross-surface Q*, description-drift S*, redaction R)

(*Q and S died on the LLM-proxy daily limit before completing — re-run them; their
prompts are in the campaign notes. R completed.)

### I1. OPEN — load_dump quick summary misleads on suspect classes (agent T, F1)
- The load banner's "Top Leak Suspects" are drawn from the histogram's top classes by
  retained, not from the leak-suspect detector: on dump_1_mnemonics the banner names
  Thread/HashMap classes while the actual suspects are `HashMap$Node[]` / `Object[]`
  single objects. Users/agents reading only the load summary investigate the wrong classes.
- Repro: `heap load tests/fixtures/dump_1_mnemonics.hprof` banner vs
  `heap report <dump> --section leaks` suspects.

### I2. DOC-GAP — `SUM(@retainedHeapSize)` double-counts shared dominatees (agent T, F3)
- OQL SUM over retained sizes counts shared children once per holder; histogram retained
  uses single-ownership dominator accounting. Numbers are not comparable and agents use
  them as cross-checks. Document prominently in OQL docs and in the query result hint.
- Repro: `SELECT SUM(@retainedHeapSize) FROM java.util.HashMap$Node` vs histogram row.

### I3. OPEN — overview/leaks sections still exceed usable MCP size on an 11 MB dump (agent T, F5)
- `get_report("overview")` = 763,833 chars / 28,828 lines; `get_report("leaks")` = 60.2 KB.
  The 256 KB cap (C3) makes them stub out, but a condensed/paginated mode for these two
  core sections would serve agents better than a stub.

### I4. DOC-GAP — DirectByteBuffer off-heap total not verifiable without graph (agent T)
- The triage's 134–162 MB native figure comes from summing `capacity` fields; without
  with_graph it can't be cross-checked by OQL. Document the requirement next to the signal.

### I5. OPEN — empty-collection signal overstates on benchmark dumps (agent T, F7)
- 72–83% of tracked collections "empty" are JDK/Scala singleton sentinels (Nil,
  Collections.emptyList()) — intentional, zero-cost. The signal should exclude known
  singleton sentinel objects or soften the recommendation.

### I6. Redaction pipeline verified on a real app dump (agent R — positive)
- lean redact 16.5 s / complete 25.1 s on clinic; check-redacted ~70 ms; detect-secrets
  30→2 (lean) / 0 (complete). Issues: lean leaves JVM symbol-table strings (2 API-key-like
  property NAMES survive); complete mode shifts suspect rankings (ReportEngine →
  SessionStore) — confirmed on a real app, so "complete-redacted dumps must not be used for
  structural leak analysis" should be printed by the tool; detect-secrets Alibaba rules
  produce 24/30 false positives on Spring Boot apps (signal-to-noise).

### I7. OPEN — (from J, needs re-verification after fixes) views requiring `collections`
- `static-collections`/`unclosed-resources` views need an undocumented load flag; either
  make the views degrade gracefully with a hint, or teach list_views to mark them.

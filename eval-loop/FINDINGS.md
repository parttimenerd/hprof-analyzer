# Eval-loop findings — hprof MCP/heap gaps surfaced by blind diagnosis

Loop: generate heap dumps from `leak-demos/` (ground truth in each demo header) →
drive the `hprof` MCP via opencode (`localproxy/gpt-5-mini` through a local
`translate_proxy.py` on :6656 that rewrites `max_tokens`→`max_completion_tokens`)
→ compare the model's blind diagnosis to ground truth → fix the gap in
hprof-analyzer. See `run_one.sh` / `run_all.sh` / `opencode.json`.

## Environment notes (so this is reproducible)
- External models: litellm host = NXDOMAIN (VPN-gated, unreachable here);
  opencode-go cloud = 402 no credits; localproxy :6655 chat path is
  `/openai/v1/chat/completions` (NOT `/openai/chat/completions`), and its GPT-5/6
  models reject `max_tokens` (need `max_completion_tokens`). Fixed with the tiny
  `translate_proxy.py` shim; harness `opencode.json` points at it.
- Pre-warm each dump's cache (`heap report <dump> --section triage`) before an
  eval so the MCP `load_dump` is ~1s not minutes.

## GAP 1 (confirmed) — static-field leak misattributed to `Thread`; owner not named

Dump: `01_StaticCacheLeak` (ground truth: static `CACHE` HashMap in
`_01_StaticCacheLeak` retains ~100% of heap).

What the tools say (`get_report({section:leaks})`):
- Headline suspect = **`java.lang.Thread` — retained 62 MB**. `root_path` is a
  single `[Thread]` entry (`root_type_label: "Thread"`). The real culprit — a
  **static field** holding the HashMap — is never named. `path` is
  Thread → HashMap(6449) → Node[], treating the main thread as the retainer.
- No `via_field` / owning-class on the root-path step (the LeakCanary
  `category`/field plumbing doesn't populate the static-field name here).
- The analyst must guess the owner via raw `SELECT @displayName FROM
  java.lang.Class` — gpt-5-mini floundered exactly here and never produced a
  final VERDICT/CULPRIT/MECHANISM.
- `find_incoming_references` (which would show the static-field edge) requires
  `with_graph=true`, which the default `load_dump` does NOT build → extra
  friction for the single most common leak pattern.

Fix direction (to verify + implement):
1. Static-field-held accumulation points should be surfaced as the suspect (or
   at least named in the root path as `<Class>.<field>`), not collapsed to the
   dominating `Thread`.
2. The root path / suspect should name the owning class + static field for a
   GC-root-via-static-field retention, so "which static field leaks?" is
   answered without manual Class-table spelunking.

CONFIRMED with graph (`find_incoming_references(6449)`): the leaking HashMap has
exactly 2 referrers — `java.lang.Thread`(4755, a transient stack local) and
`java.lang.Class`(6451, edge_count 1 = a STATIC FIELD). The static-field Class
ref is the real durable GC root; the Thread is a red herring (main still on the
stack at dump time). Yet the report's suspect/root_path = "Thread".
Additional facet: `inspect_object` on the Class(6451) shows inbound refs but
NOT the class's own name nor its static-field table — so even after finding the
Class you can't read "static field CACHE → HashMap" from the tools.

(More gaps to be appended as dumps 04/05/21/… are evaluated.)

## GAP 1 — fix plan (investigated, ready to implement)

Data already in the pipeline:
- `Graph.class_obj_class_idx: HashMap<u32,u32>` — identifies which object indices
  ARE `java.lang.Class` objects and which histogram class-row they represent
  (→ name the owning class).
- `Graph.fields_by_size_raw: Option<Vec<FieldSizeRaw>>` — raw `Class#field →
  pointee-index` records from `fielddecode.rs` (populated under `--collections`).
  This is the static-field → object attribution needed to name `<Class>.<field>`.
- `Graph.fwd_offsets` / `fwd_targets` — always-present forward CSR (fallback:
  scan Class objects' out-edges to find the one pointing at the accumulation).

Fix (in `build_leak_suspects`, src/report/build.rs ~3915):
1. For each suspect, take its accumulation object (`accumulation_obj_1based`) or
   the suspect obj; check whether a `java.lang.Class` object references it via a
   static field (look it up in the Class#field→pointee map, or scan Class
   out-edges). If so, this is a static-field GC root.
2. Add an optional `owner_static_field: Option<String>` (e.g.
   `_01_StaticCacheLeak.CACHE`) + `owner_class` to `Suspect`/`RootPathStep`
   (model.rs, `#[serde(default, skip_serializing_if)]` for golden-parity), and
   populate it. Prefer the static-field owner over a transient `Thread` stack
   local when BOTH reference the accumulation (the Class ref is the durable root;
   the Thread is a dump-time red herring).
3. Surface it in the leaks render + MCP get_report(leaks) so the answer to
   "which static field leaks?" is in the report, no Class-table spelunking.
4. Bump cache `VERSION` in src/cache.rs (changes serialized Report content).
   Add a scenario test (tests/scenarios.rs) asserting 01_StaticCacheLeak's
   suspect names the static-field owner, not just "Thread".

This is a real report-logic change (not a one-liner) — implement as its own
focused commit with the scenario-test guardrail, then re-run the blind eval on
01 to confirm the model now nails VERDICT/CULPRIT/MECHANISM.

### GAP 1 — implementation status (2026-11-23)

DONE (committed): the model fields `Suspect.owner_static_class` /
`owner_static_field` (serde skip-if, golden-safe), schema regen, cache VERSION
20→21, and equivalence wiring — the Markdown leaks render prints
"Reachable from static field `Class.field`" when set, and the JSON paths
(MCP `get_report(leaks)`, HTTP `/report`, `heap report --json`) carry the
fields automatically. So all transports are equivalent and render-ready.

CORRECTED ROOT CAUSE (disproved the first plan): the static-field→object edges
needed to POPULATE these fields are NOT available in `build_leak_suspects`. The
forward CSR (`g.fwd_offsets`/`fwd_targets`) is freed BEFORE `build_model` runs
(main.rs ~3815/6009 — an intentional RSS optimization; the CSR is multi-GB on
big heaps), so scanning Class-object out-edges there finds nothing. Verified:
owner fields come back empty even in `--with-graph` mode, because the report is
still built after the CSR free.

REMAINING (the actual data population — its own task): precompute a compact
`static_owners` map in **pass2** where the forward edges + field names are live
(src/pass2/mod.rs ~1294–1423, during fwd_targets/fwd_field_name_idx build):
record `(pointee_dense_idx → (class_row, field_name_idx))` for each Class
object's static reference field. Store it on `Graph` + persist in the cache
(bump VERSION again), then read it in `build_leak_suspects` to fill the fields
and PREFER the static owner over a transient `Thread` as the headline root.
RSS: bounded by #static-ref-fields (small vs heap), so no regression. Add a
`tests/scenarios.rs` guard (01_StaticCacheLeak names the owner, not "Thread")
and re-run the blind eval on 01.

## Step 2 (breadth) — additional dumps

### 04_UnboundedCollectionLeak (ground truth: static EVENT_LOG ArrayList→Object[]→LogEntry)
Blind model result: CORRECTLY found "java.util.ArrayList retaining ~59.3 MB
with an Object[] child of many `_04_UnboundedCollectionLeak$LogEntry`
instances" via the dominator tree — tools worked well here (unlike the
static-cache case; the dominator tree surfaces an unbounded-collection leak
clearly). Run was truncated mid-investigation (tool-budget/debug-binary
slowness) before a final VERDICT, so NOT a clear tool gap — note the same
"starts at Thread, must browse down" friction GAP 1 addresses (the GAP 1 fix
now names the static EVENT_LOG owner for this dump too, once re-analyzed with
the new binary). Weaker signal than GAP 1; no new fix warranted from this run.

NOTE: eval runs currently use the DEBUG binary (release was cleaned during the
ENOSPC reclaim); debug load/tool latency is high and truncates model runs. For
clean breadth evals, rebuild release and re-point eval-loop/opencode.json.

## GAP 1 follow-up: field NAME for statics (partial)

Graph mode (`heap load --with-graph` / MCP with_graph:true) now forces
`obj_graph=true` into pass2 (lib.rs, analyze_to_report_inner_with_cache) so
per-edge field names ARE captured (bounded by FIELD_NAME_EDGE_CAP) — this gives
instance-field root-path edges their names and was needed for owner_static_field.
BUT owner_static_field is still empty for the static-cache case: a Class object's
STATIC-field edges are emitted in fill_heap_2b WITHOUT a field-name-pool index
(only instance-field edges get annotated), so static_owners records name_idx 0.
Resolving the static field NAME requires annotating static-field edge names in
the Class-dump decode (fill_heap_2b) — a deeper hot-path change, deferred. The
owning CLASS (the primary, actionable signal) works across all transports.

## Token-efficiency run (2026-11-25, fixed release binary, 6 Haiku agents)

Naive source-blind Haiku agents, one per dump, tasked only "diagnose, fewest
calls, report efficiency metrics". All six verdicts CORRECT (incl. dump 23 now
HEALTHY — the former false-positive, fixed by the cache-shape triage softening).

| Dump            | Verdict  | Calls | load_dump sufficed alone? |
|-----------------|----------|-------|---------------------------|
| 01 StaticCache  | LEAK ✓   | 2     | no — 2nd call only for static owner name |
| 04 Unbounded    | LEAK ✓   | 2     | no — 2nd call only for static owner name |
| 05 ThreadLocal  | LEAK ✓   | 2     | no — 2nd call for root_path chain |
| 18 Oversized    | WASTE ✓  | 3     | mostly (1 wasted inspect_object) |
| 23 Healthy      | HEALTHY ✓| 2     | mostly (2nd call "prudent, not necessary") |
| 29 Collision    | PERF ✓   | 1     | YES |

DOMINANT TOKEN SINK: the mandatory 2nd `get_report(leaks)` call (~8 KB, up to
1900 lines, mostly the deep dominator_tree) purely to recover the static-field
OWNER name that load_dump's inlined headline omitted. load_dump already had the
owner (GAP-1 `owner_static_class`) but only printed it in the Warning (softened)
branch, not the Critical branch.

FIX APPLIED: `static_owner_suffix()` appends ", held by static field
`Owner.field`" to the Critical headline-retainer signal whenever the suspect has
a static owner — so load_dump's inline verdict names the real culprit for ALL
static-held leaks. Expected effect: 01/04/23 drop from 2 calls to 1.
VERSION bumped 22→23 (triage text changed).

SECONDARY: get_report(leaks) dominator_tree is verbose (agents "skim" the
dominated[] arrays). Candidate future trim: a compact/own `leaks` mode that
drops the deep tree when the owner+root_path already answer the question.

## Harder-dump sequential round (2026-11-25, final binary)

Sequential (one agent at a time), harder scenarios: 1 real-world fixture + 3
indirect/deep leaks. All verdicts CORRECT.

| Dump                  | Verdict   | Calls | Note |
|-----------------------|-----------|-------|------|
| dump_2_scala-doku(real)| HEALTHY ✓ | 4     | deep chain flagged WARNING not Critical — no false positive on real diffuse heap |
| 02 ListenerLeak       | LEAK ✓    | 2     | indirect: static EventBus → 12,453 lambda listeners |
| 25 DeepObjectChain    | LEAK ✓    | 3     | 50k-node linked list; wanted field-name `next` inline |
| 33 AppEventPipeline   | LEAK ✓    | 3-4   | static ArrayDeque → 50k Events |

GENERALIZABLE NEXT LEVER (3 of 4 agents, independently): load_dump's inline
headline now names the static OWNER, but agents still spend a 2nd/3rd call
(get_report leaks) to recover the ACCUMULATION CHAIN — holder → collection →
accumulated-type with instance count — and the field edge names (`next`,
`eventBus`) and the accumulation-point object index. Candidate trim: inline a
COMPACT root-path/accumulation chain (3-4 hops, with field names + the accum
object index + accumulated-instance count) in load_dump, so indirect/chain/
deep leaks also resolve in 1 call. No false positives or wrong culprits in this
round; no tool failed — the gap is purely "one more call for the chain detail".

## Measured hard round (2026-11-25, build7: chain-inlining + trimmed descs + help-pointer)

Sequential, token-measured (subagent_tokens / tool_uses from completion notices).

### 17 PersistenceContextLeak — LEAK ✓ but 7 tool calls (HARDEST so far)
Correct culprit (FakeSession.firstLevelCache HashMap, 30K entities) but expensive:
needed load_dump → leaks → get_dominator_path → load_dump(with_graph=true) →
find_incoming_references → inspect_object ×2. ROOT CAUSE OF COST: the leak is held
via Thread.scopedValueBindings → Lambda closure → FakeSession.firstLevelCache —
a THREAD-LOCAL/closure path, NOT a static field. So owner_static_class is
(correctly) empty and the agent had NO owner shortcut → manual graph traversal.

NEW GAP (distinct from the static-field cases): thread-local / scoped-value /
closure-held leaks have no owner attribution. GAP-1 only names STATIC-field
owners. Candidate: when the dominator is a Thread and the accumulator is held
via a ThreadLocal/scoped-value/lambda-captured field, surface that holding field
(e.g. "held via Thread-local `FakeSession.firstLevelCache`") the same way
owner_static_* surfaces static fields — so thread-local leaks also resolve
without 5 inspect_object calls.

### 37 ThreadLocalPooledThread — PARTIAL MISS (61.4k tok, 5 calls)
Small heap (9.7 MB on-heap, no dominant on-heap leak: headline String 29% below
floor, concentration "diffuse"). Agent latched onto the 137.6 MB DirectByteBuffer
and called it "OOM risk via sun.nio.ch.Util$BufferCache ThreadLocal" — but that
buffer is largely the BENIGN JDK boot-image / NIO cache
(jdk.internal.jimage.ImageReader), not the demo's intended subtle pooled-thread
leak. Over-called a benign off-heap cache.

RECURRING OFF-HEAP FALSE-ATTRACTION: the "off-heap: 137.6 MB DirectByteBuffer"
WARNING appears on nearly EVERY dump (same JDK boot-image buffer) and repeatedly
tempts agents toward it as "the leak", especially when the real on-heap signal is
faint. Candidate fix: the off-heap signal should distinguish the known benign JDK
boot-image/NIO buffer (jdk.internal.jimage.ImageReader$SharedImageReader, a fixed
~base cost) from genuinely app-owned DirectByteBuffer growth — e.g. subtract/annotate
the boot-image buffer so a healthy heap's off-heap line doesn't read as a leak.

### 11 WeakHashMapLeak — LEAK ✓ (68.7k tok, 5 calls) — static owner inline worked
### 32 AppSessionStore — NOT RUN (Haiku proxy hit €50/day limit mid-run; resets UTC midnight)

## Measured token usage (build7, per-diagnosis subagent_tokens / tool_uses)

| Dump                   | Verdict        | Correct | tool_uses | subagent_tokens |
|------------------------|----------------|---------|-----------|-----------------|
| 17 PersistenceContext  | LEAK           | ✓ hard  | 12        | 66.7k |
| 37 ThreadLocalPooled   | OOM-risk       | partial | 5         | 61.4k |
| 11 WeakHashMapLeak      | LEAK           | ✓       | 5         | 68.7k |

Earlier round (6 agents, static/real dumps): correct 6/6, 1-4 calls, ~57-73k tok.

READING: subagent_tokens is dominated by the agent's system prompt + reasoning,
NOT our tool output — it sits ~57-73k regardless of 1 vs 12 calls, because each
tool round re-sends the (now-trimmed) context. The lever that actually moves cost
is TOOL_USES (round-trips): static-field leaks = 1-3 (owner+chain inline);
thread-local/closure leaks = 5-12 (no owner shortcut → manual graph walk). So the
two open gaps both target tool_uses on the hard cases:
  1. thread-local/closure owner attribution (would cut 17 from ~7 MCP calls to ~2)
  2. de-emphasize the benign JDK boot-image DirectByteBuffer off-heap signal
     (stops agents chasing it on faint-signal heaps like 37)

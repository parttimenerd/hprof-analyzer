# Visualization ideas — short summary

Index of the 62 remaining original ideas (G4, N5, N6 removed: they need data not in the dump), plus P1–P4. Full assessments (pros/cons, gates, size, Rust
cost): [VISUALIZATION_IDEAS.md](VISUALIZATION_IDEAS.md). Ideas that are not
part of the default flow: [VISUALIZATION_IDEAS_DEFERRED.md](VISUALIZATION_IDEAS_DEFERRED.md).
The [approved decision record](VISUALIZATION_IDEAS.md#decision-record-2026-11-04)
supersedes conflicting proposal wording; approvals are **not implementations**.

**Ground rules:** one report; sections are hidden only when their data is not
available. Use only data from the heap dump: no RSS, GC logs, `jcmd`, or
sidecars. Retained bytes overlap, so only shallow bytes are summed.

**Totals:** 54 kept (core, merged, or contextual), 8 separate, 4 new
pool/finalizer ideas (P1–P4). The verdict table with a "shown when" column
for every ID is in [VISUALIZATION_IDEAS.md](VISUALIZATION_IDEAS.md#user-verdicts).
Keep navigation task-first, **without a hard destination cap**; reuse the
existing inspector/sidebar instead of introducing a duplicate detail panel.

## Overview

| ID | Idea | Decision |
| --- | --- | --- |
| C5 | Retention concentration (top-1/10/100 share) | Merge |
| H1 | Fix-ROI ranker → evidence-based priority | Replace |
| H2 | Deterministic triage digest (≤3 headline findings) | Keep |
| N3 | Heap-health KPIs → plain measured facts | Replace |

## Consumers (what occupies the heap)

| ID | Idea | Decision |
| --- | --- | --- |
| C1 | Category composition (shallow, additive) | Merge |
| C2 | Framework footprint + decoded cache config | Narrow/merge |
| C3 | Per-instance size scatter | Merge |
| C4 | Size distribution | Correct/merge |
| F1 | Loader scope, zero-direct-instance classes | Contextual |
| F2 | Same class loaded by several loaders | Merge |
| F3 | Selected application-family counts/additive shallow bytes (not summed retained) | New producer, contextual |
| I2 | Package retention as a scope filter | Merge |
| K3 | Multi-mode scatter explorer | Simplify |
| K5 | Group instances by a business field value | Keep (guided query) |

## Owners (why it is retained)

| ID | Idea | Decision |
| --- | --- | --- |
| A1 | Dominator sunburst (small, selected subtree) | Contextual mode |
| A2 | Holder → payload class pairs | Keep (table first) |
| A3 | Big-drop step-down remainder | Merge |
| A5 | Same-class dominator nesting | Merge |
| B1 | Retention Sankey (small ownership pairs only) | Narrow/merge |
| B2 | Observed alternate referrers / reference-cut evidence | Keep evidence and explicit cut query; drop separate automatic caution |
| B3 | GC-root-type attribution | Merge |
| K1 | Top retainers | Merge |
| K2 | Fields by retained size | Merge |
| K4 | Lifetime annotations, with no "safe/leak" labels from package names | Replace labels |

## Collections and optimization opportunities

| ID | Idea | Decision |
| --- | --- | --- |
| D1 | Waste summary (merged with J5) | Merge |
| D2 | Collection capacity vs. usage | Contextual mode |
| D3 | String content and compact storage (encoding-aware) | Narrow/contextual |
| D4 | Tiny/empty collection overhead by field | Keep |
| D5 | Collection content types | Merge into detail |
| J1 | Boxed-number footprint | Contextual |
| J2 | Object-header overhead estimate | Contextual |
| J3 | Duplicate primitive arrays | Keep contextual |
| J4 | Constant-value primitive arrays | Narrow/contextual |
| J5 | One merged optimization-opportunity list | Keep |
| J7 | Throwables and stored stack traces | Contextual query |
| L1 | Collection kind composition | Merge into filter |
| L2 | Map bucket occupancy, with real chains only via a query | Correct proxy |
| L3 | Largest individual arrays + owners | Keep |
| L4 | Largest held collections | Merge |
| N1 | Large-array facts (no guessed G1 humongous regions) | Merge into L3 |

## Threads

| ID | Idea | Decision |
| --- | --- | --- |
| E1 | Thread state (neutral context) | Merge into E3 |
| E2 | Stale ThreadLocal values | Keep |
| E3 | Per-thread retained memory | Keep |
| E4 | Stack locals, depth, repeated frames | Merge into detail |
| N4 | Per-thread retained chart (duplicate of E3) | Merge |

## References, buffers, finalization

| ID | Idea | Decision |
| --- | --- | --- |
| B4 | DirectByteBuffer capacity and owners | Contextual |
| E5 | Monitor-rooted objects | Narrow/merge |
| I1 | Soft/weak/phantom reference evidence | Keep contextual |
| N2 | Finalizer population (not proof of queue backlog) | Narrow/contextual |

## Pools and finalization (new)

| ID | Idea | Decision |
| --- | --- | --- |
| P1 | Supported pool layouts: size, workers, queue snapshot | Keep |
| P2 | Finalizer referents by class | Keep |
| P3 | Cleaner registrations by action; resource if resolvable | Contextual |
| P4 | Verified direct-buffer unique backing capacity | Keep (corrects B4 when supported) |

## Compare (multiple dumps)

| ID | Idea | Decision |
| --- | --- | --- |
| G1 | Signed population/shallow-byte deltas and comparable-capture trends | Keep; not leak proof or OOM forecast |
| G3 | Type-edge delta table | Contextual |

## Evidence

| ID | Idea | Decision |
| --- | --- | --- |
| H3 | Coverage, limits, provenance | Keep |
| I3 | Unreachable-object composition | Contextual |
| M2 | Decoded JVM version/properties | Narrow/contextual |

## Separate (not in default flow)

| ID | Idea | Why separate |
| --- | --- | --- |
| A4 | Immediate-dominator chord diagram | A2's table answers the same question better |
| A6 | Whole-heap cycles / SCC explorer | Costly; cycles are not leak evidence |
| D6 | Field null-rate / layout inspector | Only useful to specialists |
| G2 | Animated diff treemap | Decorative; motion hides what changed |
| I4 | Allocation flame graph | Most dumps have no allocation traces |
| J6 | Duplicate arbitrary value objects | Costly, and equal values are not always safe to share |
| M1 | Secret-scanner dashboard | Belongs to the sharing workflow |
| M3 | HPROF record-composition sunburst | Record counts are not bytes |

## Shared UX and triage decisions

- **X1–X2:** Brief hover/keyboard-focus explanations complement the existing
  sidebar; consolidate inspector surfaces, do not build a second panel.
- **X3–X6:** A context menu is optional, never the sole action; keep explicit
  selection/scope, bounded on-demand ownership versus reference paths, and
  merge “Why retained?” into the inspector.
- **X7:** Share only essential metric/scope labels and relevant inspect/link/copy
  behavior; no universal menu/action checklist for every chart.
- **Correct existing claims:** bucket occupancy is not collision-chain length;
  large arrays do not establish G1 regions; registered Finalizers do not prove
  pending backlog; recorded unreachable bytes are not fragmentation or GC
  throughput; thread retention does not prove stale ThreadLocal value bytes;
  name-suffix exceptions and loader/native claims need verified evidence.
- **Keep as refinements:** one grouped coverage-limits notice; verified distinct
  stale ThreadLocal value footprint; field-specific empty-container overhead;
  owner-specific large-array facts; selected F3 family footprint; registered
  Finalizer class breakdown. Latin1-compatible wide String accounting remains
  [deferred](STRING_METRICS_IDEAS.md).
- **Dropped triage proposals:** separate shared-retainer caution, business-
  bucket footprint skew, and queued-work retention. Their B2/K5/P1 investigative
  views remain; a queue snapshot is not an alert.
- **No automatic alert** from cycles, WAITING/BLOCKED state, unknown cache
  policy, zero-instance classes, high retained/shallow ratio, address gaps,
  hypothetical G1 regions, or time-to-OOM extrapolation. These are not leak
  proof; retain measured facts in relevant detail views.
- **Order/gates:** correctness and coverage first, then existing useful slices,
  contextual modes, P2/P4 before descriptive P1, and size more expensive
  producers on ThinkStation. Deferred specialist ideas stay separate. Check
  additive totals, actual entry-point capabilities, accessibility/navigation,
  production-rule tests, and measured Rust memory/runtime before release.

## Memory pools and finalizers: current coverage

| Area | What is analyzed today | Gap (still dump-only) |
| --- | --- | --- |
| DirectByteBuffer | Capacity sum, count, 8 size buckets, top-20 owners with root paths | Slices/duplicates are not deduplicated by shared backing (`att`) |
| Framework pools (ThreadPoolExecutor, HikariCP, Netty) | Instance count and retained total via class-name sentinels | Pool size, active/idle entries, queue length, and Netty arena usage are not decoded from fields |
| Executor/scheduled tasks | Existing class-histogram/thread evidence (not a queue-retention alert) | Queue depth per executor is not read from `workQueue` |
| Finalizer | `java.lang.ref.Finalizer` instance count (triage at ≥10,000); `unfinalized` edge excluded from dominators | No per-referent-class breakdown; pending queue vs. registered population is not separated |
| Cleaner / PhantomCleanable | Counted only as generic phantom references | No Cleaner-specific view by resource class |

These gaps can all be filled from fields and objects that are already in the dump.

## User verdicts

Each line answers what a user debugging a big heap or an OOM gets from the idea.
Ratings: **Must-have** (answers "what/why" directly), **Helpful** (speeds up the
next step), **Nice-to-have** (occasional), **Skip** (adds noise for most users).

### Overview

- **H2 Triage digest — Must-have.** "Tell me the three things to look at first."
- **H3 Coverage/limits — Must-have.** "Tells me when a clean result just means it wasn't checked."
- **C5 Concentration — Helpful.** "One number tells me whether I'm hunting one culprit or many."
- **H1 Evidence priority — Helpful.** Useful only as an ordering; fake ROI scores would mislead.
- **N3 Measured facts instead of a health score — Helpful.** A single health score hides what is actually wrong.

### Consumers

- **C1 Category composition — Helpful.** Quickly shows whether my code or a library dominates.
- **C2 Framework footprint — Helpful.** "Is Hibernate/Spring holding this?"
  - *Decoded cache config (max size, TTL):* Helpful when present.
  - *Inferred hit rate / "broken eviction":* Skip, because the dump can't show it.
- **C3 Per-instance size scatter — Nice-to-have.** Finds unusually large objects; the class table usually suffices.
- **C4 Size distribution — Nice-to-have.** Context only.
- **F1 Loader scope — Helpful** for app-server/plugin users; **Skip** for everyone else.
- **F2 Duplicate loaded classes — Helpful** when separate definitions exist; no redeploy-leak claim from this alone.
- **F3 Superclass rollup — Helpful** for a selected application family, with additive shallow bytes and descendant counts only.
- **I2 Package scope — Helpful** as a filter, not as yet another treemap.
- **K3 Scatter explorer — Nice-to-have.** One mode is enough.
- **K5 Group by field value — Must-have** for multi-tenant apps. "Tenant 42 holds 70% of sessions."

### Owners

- **A2 Holder → payload pairs — Must-have.** "Who is holding all these `byte[]`?"
- **K1 Top retainers — Must-have.** The usual entry point to the investigation.
- **K2 Fields by retained size — Must-have.** Names the exact field to fix.
- **B3 Root-type attribution — Helpful.** Static vs. thread vs. JNI roots point to different fixes.
- **A1 Dominator sunburst — Helpful** for a selected subtree; **Skip** as a whole-heap chart.
- **A3 Big-drop remainder — Helpful.** Shows where retention fans out.
- **A5 Same-class nesting — Helpful.** Spots linked lists and recursive structures.
- **B1 Retention Sankey — Nice-to-have** at small size; a global Sankey is unreadable.
- **B2 Shared ownership — Helpful.**
  - *"Also referenced by…" evidence:* Useful investigation context, without a separate automatic caution.
  - *Exact "cut this edge, free X":* Helpful but costly; run it only on request.
- **K4 Lifetime annotations — Helpful** with measured facts; **Skip** "JDK = safe" labels.

### Collections and optimization opportunities

- **J5 One opportunity list — Must-have.** One ranked list instead of eight charts.
- **L3 Largest arrays + owners — Must-have.** Concrete, actionable allocations.
- **D4 Empty/tiny collections by field — Must-have.** Names the field to lazy-init.
- **J3 Duplicate primitive arrays — Helpful.** Clear dedup target when large.
- **D2 Capacity vs. usage — Helpful.** Finds oversized maps and lists.
- **D3 String content/encoding — Helpful** for duplicate strings; compact-storage savings only after encoding-correct accounting.
- **J1 Boxed numbers — Helpful** when large; otherwise hidden.
- **L2 Map buckets — Nice-to-have.** Real collision chains only via explicit query.
- **J7 Throwables/stack traces — Helpful** when exceptions are being kept around.
- **D1 Waste summary — merged into J5.** No separate value.
- **D5 Collection contents — Helpful** inside the detail drawer.
- **J2 Header overhead — Nice-to-have.** Educational; rarely the fix.
- **J4 Constant arrays — Nice-to-have.**
- **L1 Collection kinds — Nice-to-have** as a filter.
- **L4 Largest held collections — merged.** Same answer as D2/L3.
- **N1 Large-array facts — Helpful** as part of L3; **Skip** guessed G1 humongous labels.

### Threads

- **E3 Per-thread retained — Must-have.** "This worker thread holds 2 GB."
- **E2 Stale ThreadLocals — Must-have** when they are present. A classic pool leak with a clear fix.
- **E4 Stack locals — Helpful.** Shows which frame holds the payload.
  - *Repeated-frame grouping:* Helpful for reading deep stacks.
  - *Automatic "runaway recursion" verdict:* Skip.
- **E1 Thread state — Nice-to-have** as context; WAITING is normal for pools.
- **N4 Second thread chart — Skip.** Duplicate of E3.

### References, buffers, finalization

- **B4 DirectByteBuffer owners — Must-have** when buffers are large. Shows reported capacities and heap owners, not measured native-memory consumption.
- **I1 Soft/weak/phantom — Helpful.** Explains "why didn't GC free this cache?"
- **N2 Finalizer population — Helpful** on legacy code; a per-class breakdown would make it actionable.
- **E5 Monitor roots — Nice-to-have.** Rare; not a lock analyzer.

### Compare

- **G1 Changes across dumps — Must-have** as evidence of observed growth, not leak proof. Two dumps show only a delta, not a trend.
- **G3 Type-edge deltas — Helpful** when growth needs explaining ("which holder gained the references?").

### Evidence

- **I3 Unreachable composition — Helpful.** Describes recorded unreachable objects without asserting a capture phase or GC problem.
- **M2 JVM properties — Nice-to-have.** Context for interpreting the rest.

### Separate ideas

- **A4 Chord — Skip.** A2's table is clearer.
- **A6 Global cycles — Skip.** Cycles are normal in Java.
- **D6 Null-field layout — Skip** for most users.
- **G2 Animated diff — Skip.** Motion hides the change.
- **I4 Allocation flame graph — Must-have** when traces exist; **hidden** otherwise (most dumps).
- **J6 Arbitrary value duplicates — Nice-to-have** for specialists.
- **M1 Secret scanner — Helpful** before sharing a dump, but outside the report.
- **M3 Record sunburst — Skip.**

### Pools and finalization

- **P1 Pool state — Must-have.** Shows decoded queued work and owner paths without a queued-work alert; one snapshot cannot prove exhaustion.
- **P2 Finalizer referents by class — Must-have.** Names registered classes without claiming pending or unclosed.
- **P3 Cleaner by resource — Helpful.** Shows decoded action and resource when the referent is available.
- **P4 Unique direct-buffer capacity — Helpful.** Avoids slice double-count only with verified backing identity.

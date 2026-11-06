# Separate new views and ideas — reviewed, not forgotten

Companion to [VISUALIZATION_IDEAS.md](VISUALIZATION_IDEAS.md). This file keeps
separate new views/ideas that are not part of the default report flow today.
They are still valid concepts, but they need extra evidence, specialist context,
or higher implementation cost to answer **what occupies this heap and why is it
retained?**

The audit covers **8 whole ideas** here and **54 retained/merged ideas** in the
main catalog. G4, N5, and N6 were removed because they need data that is not
in the heap dump. Externally dependent slices of
retained ideas are listed separately below; they are not additional idea IDs.

The [2026-11-04 decision record](VISUALIZATION_IDEAS.md#decision-record-2026-11-04)
is authoritative for later approvals and drops. These eight specialist ideas
stay separate until their re-entry criteria below are met; approval of a
related main-catalog view does not authorize implementation of a deferred idea.
Physical String slack, sharing, and savings are tracked separately in
[STRING_METRICS_IDEAS.md](STRING_METRICS_IDEAS.md). There is **no hard cap** on
report destinations; new destinations must still earn their place by making
investigations clearer.

## Cost conventions

All size figures below are **unmeasured planning budgets**, not benchmark results.
`ΔJSON` means additional raw serialized report data; gzip UI size means marginal
compressed JavaScript/CSS when an implementation is actually added. Reusing an
existing report field adds **0 B data**, even if enabling its producer is expensive.

`N` = heap objects, `E` = references, `C` = classes, `B` = bytes examined,
`S` = selected objects, `D` = distinct signatures, `R` = capture count.
Example arithmetic uses **N=10 million, E=30 million**; it is not a measured heap.
A JSON property costs its key, punctuation, and decimal value—not four/eight bytes
just because its Rust field is `u32`/`u64`. Shared UI budgets are not additive.

## Separate ideas (not in default flow)

### A4. Immediate dominator chord diagram

- **Verdict:** Defer — unlikely to improve an investigation beyond A2's searchable pair table/optional small heatmap.
- **Analysis:** Basic already supplies immediate-dominator pairs; Full is unnecessary for those pairs.
- **Pros:** Can expose a small, curated relationship topology.
- **Cons:** Long class labels, crossings, and angular comparisons obscure ranking; class-retained links are not conserved memory flow.
- **Improve:** Revisit only for a demonstrated user task that a filtered 10×10 matrix cannot answer; never make it a default chart.
- **Size:** ΔJSON 0 B; estimated 4–12 KiB gzip custom UI, plus any newly chosen chord library. No package is justified now.
- **Rust:** 0 additional passes or heap scratch for re-rendering existing pairs. Chart layout is browser work; source pair aggregation is already paid in Basic.

### A6. Whole-heap reference cycles / SCC explorer

- **Verdict:** Defer — heap-only but specialist; high compute cost and a weak leak discriminator in a tracing-GC language.
- **Analysis:** Requires a new explicit SCC pass over a sufficiently complete graph. Existing CLI Full's bounded exported graph is not an exhaustive SCC input.
- **Pros:** Useful for understanding a selected bidirectional entity/cache graph.
- **Cons:** Unreachable cycles are collectible; reachable cycles can be intentional. A capped graph splits real components and cannot prove their absence.
- **Improve:** First provide ordinary owner/root exploration. Consider selected-subgraph SCCs only on an expert request, with scope and strong-reference policy shown.
- **Size:** Proposed summaries ≤32 KiB raw for 20 components without full member export; estimated 4–12 KiB gzip incremental graph UI using the existing engine.
- **Rust:** O(N+E) for a global pass. Indices, lowlinks, node stack, and membership alone can need about `12N + N/8` bytes (~116 MiB at the example N); component labels, iterative traversal frames, and retained adjacency add more. This is a lower-bound working-set example, not a promise of 2× heap size or a seconds estimate.

### D6. Field null-rate / object-layout inspector

- **Verdict:** Defer — usually helps library/layout specialists, not a user tracing the large live set.
- **Analysis:** CLI `--field-stats` is separate from `--full-analysis`; MCP/browser Full does not request it. Current output targets the top 50 classes by instance count, not all fields/classes.
- **Pros:** Can identify frequently unused reference slots in a known, verified layout.
- **Cons:** High null rate is not wasted allocated child memory and does not imply a safe field removal. Validate field-slot identity when null edges are absent from the CSR; ordinal edge positions are not automatically field positions.
- **Improve:** Require decoder/slot correctness tests before recommendations, and keep the result in expert queries instead of the main report.
- **Size:** ΔJSON 0 B to render a report already made with the flag; new field results scale with reported fields, with a proposed ≤32 KiB raw UI-facing slice. Estimated 2–6 KiB gzip table UI.
- **Rust:** Enabling the current CLI path saves a forward CSR copy: about `4(N+1)+4E` bytes (~153 MiB at the example sizes), plus class/field counters. The scan is O(N+E_selected). It is not merely four extra counters or an automatically included Full feature.

### G2. Animated diff treemap

- **Verdict:** Defer — primarily decoration; likely to reduce comprehension as squarified tiles move.
- **Analysis:** Two compatible heap reports; Basic reports suffice for the class data. Animation does not require Full.
- **Pros:** Can make a very small controlled change visible.
- **Cons:** Layout motion hides which class grew; input joins by class name can combine different loaders. Animation is not object tracking.
- **Improve:** Use signed numeric deltas, G1 trends, and fixed-order side-by-side comparisons. Respect reduced motion if animation is ever reconsidered.
- **Size:** ΔJSON 0 B when the series is already loaded; estimated 2–6 KiB gzip for extra layout/state logic.
- **Rust:** 0 for CSS animation. A newly requested comparison has the existing class-join cost, O(sum of report rows), not a heap rescan.

### I4. Allocation flame graph

- **Verdict:** Defer from normal navigation — potentially valuable, but ordinary modern HotSpot dumps usually lack usable per-object allocation traces.
- **Analysis:** Basic already attempts the allocation-site aggregation, bounded by `--detail`; usable nonzero allocation serials/frames must actually be in the dump. Current options do not have a separate `--alloc-sites` boolean. Full cannot reconstruct missing allocation history.
- **Pros:** Connects surviving allocations to code locations when genuine trace data exists.
- **Cons:** Thread stacks are not allocation stacks. Summed retained weights across allocated ancestor/descendant objects can overlap; a flame width must not silently become total occupied heap.
- **Improve:** Reveal only when `traces_present` and supported frames exist; prefer allocation count, or a separately verified additive shallow metric. Reuse existing flame/icicle components instead of adding another library.
- **Size:** ΔJSON 0 B for the existing trace-bearing report; estimated 2–8 KiB gzip folding/drill-down UI. Default allocation output is top 50 sites; maximal detail is different.
- **Rust:** 0 additional work for charting existing sites. A new shallow-by-site aggregate would be O(N), with O(number of sites) counters; avoid preserving full arrays past their normal release point merely to draw this view.

### J6. Duplicate arbitrary immutable/value objects

- **Verdict:** Defer — useful in a demonstrated value-heavy workload, but an expensive and semantics-sensitive default feature.
- **Analysis:** New heap-only allowlisted fingerprint/equality pass. Current `--find-duplicates`/Full handles Strings and primitive arrays, not arbitrary DTOs or JDK value objects.
- **Pros:** May identify repeated dates/numbers/configuration values missed by existing duplicate summaries.
- **Cons:** Records and final fields do not prove deep immutability. Equality does not permit sharing when identity, synchronization, serialization, or mutable children matter. Hash matches need equality verification; redaction manufactures duplicates.
- **Improve:** Start with a small explicit layout/equality allowlist, a byte budget, and duplicate own-shallow evidence. Never claim the sum of retained copies is reclaimable.
- **Size:** Proposed ≤24 KiB raw for 25 verified groups without full payload export; estimated 2–8 KiB gzip reused duplicate-table UI.
- **Rust:** O(B) decoding plus equality-verification work. Signature state grows with D (for example 48–96 B per entry before strings, child signatures, and allocator overhead). A proposed 64–128 MiB work budget must report partial/unsupported results when exhausted, not imply a constant-cost global scan.

### M1. Secret-scanner dashboard

- **Verdict:** Defer to sharing/security tools — worthwhile for safe handling, but not a default answer to why the heap is big.
- **Analysis:** Existing `detect-secrets` workflow; Basic/Full report generation does not automatically execute or serialize that workflow.
- **Pros:** Reduces exposure of credentials and personal data during sharing.
- **Cons:** Regex findings can be false positives; a negative or redacted result is not proof that sharing is safe. A red/green security gate would overpromise and distract from retention analysis.
- **Improve:** Offer a separate explicit masked sharing check; retain only the already-known redacted-input warning in normal report coverage. Never put secret samples into ordinary report exports.
- **Size:** Proposed ≤16 KiB raw for 50 masked aggregate findings; estimated 2–6 KiB gzip UI. No added secret values or automatic security panel in the heap report.
- **Rust:** Depends on String/payload decoding and pattern count: approximately O(B × patterns), with bounded findings and potentially substantial decoder/index state. “30 LOC plumbing” is not a credible total security/scan cost.

### M3. HPROF record-composition sunburst

- **Verdict:** Defer the chart — tool developers may use it; normal users need H3's coverage facts, not another donut.
- **Analysis:** Basic already has the record census and a Dump Completeness table. Full is unnecessary.
- **Pros:** Helps explain missing records or diagnose unusual dump writers.
- **Cons:** Record counts are heterogeneous, not byte occupancy; heap segments, instance records, and root records are not an additive heap budget. `0x08` is a thread root here, not pending finalization.
- **Improve:** Keep the census table collapsed under Evidence. Build a specialized format-diagnostics view only for a concrete developer task.
- **Size:** ΔJSON 0 B; estimated 2–6 KiB gzip chart UI that is not justified for the default report.
- **Rust:** 0 additional passes/counters for displaying the existing census; file-byte composition would require new per-record-size accounting, not percentages of current counts.

## Deferred slices of otherwise useful ideas

These boundaries prevent “Full Analysis” from quietly meaning “all evidence exists.”

| Retained idea | Keep in normal report | Move out / reject without evidence |
| --- | --- | --- |
| B1 | A small ownership-pair view, absolute counts/shallow or qualified retained attribution | Type-reference `retained_weight` as conserved byte flow; global automatic Sankey without cycle/duplication handling |
| B2 | Available owner chain and observed alternate referrers | Exact single-edge savings from inbound degree/idom alone; complete-graph counterfactuals need a new explicit budgeted query |
| C2 | Detected framework footprint, supported literal cache configuration decoded from heap | Hit rate, expiry effectiveness, cache activity, or “broken eviction” inferred from occupancy alone |
| C5 | Exact existing top-1/10/100 concentration milestones | Global smooth Lorenz curve or arbitrary threshold counts invented from three points; optional full ranking is specialist work |
| E5 | Busy-monitor root counts / qualified object attribution | Owners, waiters, contention duration, deadlock proof from `ROOT_MONITOR_USED` alone |
| F1 | Loader rollup / zero-direct-instance filter; heap-decoded parent when implemented | “Unused dependency,” JAR identity, metaspace bytes, or stable cross-dump loader identity inferred from class names/addresses |
| F3 | Superclass hierarchy from captured class metadata | Implemented-interface/JAR rollups without matching extra metadata |
| G1 | Class counts/shallow/qualified retained series; header-time rates when forwarded | Time-to-OOM, physical-object age, or workload-normalized leak proof without extra evidence |
| K4 | Neutral category and actual measured/root-field annotations | Package-derived “known alive/dead,” and application=leak / framework=safe colors |
| M2 | Decoded Java version/system properties, unknowns visible | Complete VM arguments, collector, `-Xmx`, or oop width inferred from file size or ID width |
| N1 | Largest measured arrays, neutral size/capacity context (merged with L3) | Actual G1 humongous classification, region waste, GC frequency, or tuning recommendations without collector/region context |
| N2 | Recorded Finalizer population as evidence | Pending queue size or a stuck Finalizer inferred solely from registry count; optional queue-layout decoding needs separate verification |

## Re-entry criteria

1. A reproducible user investigation that existing owners/consumers/query views cannot solve.
2. Verified data from the heap dump itself. Ideas that need external telemetry
   are removed, not deferred; the report never guesses it.
3. A bounded report schema, demonstrated peak-memory/runtime behavior, and explicit incomplete results.
4. No new default report destination unless user testing shows lower investigation effort.
5. No automatic triage severity based merely on cycles, thread state, class names, or address patterns.

## Source grounding

- Option defaults and caps: [src/opts.rs](src/opts.rs).
- CLI option folding/forward-CSR copy: [src/main.rs](src/main.rs).
- MCP/browser differences: [src/mcp/mod.rs](src/mcp/mod.rs), [crates/hprof-wasm/src/lib.rs](crates/hprof-wasm/src/lib.rs).
- Producer semantics and frees: [src/report/build.rs](src/report/build.rs), [src/pass2/fielddecode.rs](src/pass2/fielddecode.rs).
- Existing String decoder/fingerprints: [src/pass2/strings.rs](src/pass2/strings.rs).
- Existing rule inventory: [src/report/triage.rs](src/report/triage.rs).

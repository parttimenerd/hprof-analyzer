# hprof-analyzer PLAN — post-v16 improvements

Items ordered by execution sequence. One commit per item. No batching.

---

## P0 — Correctness bugs (fix first)

### P0-1 — Thread triple-fire: three signals all name Thread when it's merely a transit holder

**Symptom:** Three triage signals simultaneously name `java.lang.Thread` as the culprit on a heap where Thread only transits to a deep tree:
- `headline-retainer`: "Thread retains 79.4%"
- `concentration`: "Thread holds 79.4%"
- `gc-root-type`: "79.4% held by Thread roots"

This is noise, not signal — the user already knows there's one Thread holding the heap; all three saying so redundantly is "kind of pointless."

**Root cause (`src/report/triage.rs`):**
- `HeadlineRetainer` (~line 461) falls back to `leaks.suspects.first()` when `primary_culprits` is empty — this can surface Thread
- `Concentration` (~line 539) does the same fallback
- `DominantGcRootType` (~line 598) suppresses only when `suspect_class.contains(root_type_label)` where `suspect_class` comes from `primary_culprits[0]` — but when `primary_culprits` is empty, this guard is skipped entirely

**Fix:**
1. `DominantGcRootType`: when `primary_culprits` is empty, also check `leaks.suspects.first()` for suppression (mirror what `HeadlineRetainer` uses as its fallback)
2. Mutual-suppression between `HeadlineRetainer` and `Concentration`: if both name the *same* class, `Concentration` is redundant — suppress it. The detail text for `headline-retainer` already covers the "X% held by single object" fact.

**Files:** `src/report/triage.rs` (HeadlineRetainer, Concentration, DominantGcRootType structs)

**DoD:** The existing `scenario_joint_node_thread_headline_bug` test already asserts `gc-root-type` doesn't fire — extend it to also assert that `concentration` doesn't fire when `headline-retainer` already named the same class. Add a new scenario `thread_as_transiter_below_threshold` where `primary_culprits` is empty (deep tree below the dominance floor) — assert at most one of the three signals fires.

---

### P0-2 — "Who Holds This Class?" empty for classes outside the holder-computation window

**Symptom:** The `Simple` class (and other small-dump culprits) show no holder data under "Who Holds This Class?" / `BiggestClassHolders`. The holders table is blank even when the class is clearly the only significant retention in the heap.

**Root cause (`src/report/build.rs` ~line 4913):** `build_top_consumers` computes holder data only for the top `top_class_n = top_n.min(class_order.len())` classes, where `top_n` defaults to 20. Classes ranked 21–∞ get `Vec::new()`.

**Fix:** Widen the holder-computation window to `top_n * 3` (compute holders for top-60 by default; display still caps at `top_n`). The holder pass is an O(n) linear scan — widening is cheap.

**Files:** `src/report/build.rs` (HOLDER_CAP computation and `top_class_n` assignment)

**DoD:** New synthetic scenario `single_culprit_simple_class` — a heap with one dominant custom class (`scenarios/SimpleValue`) that's rank 1 by retained. After analysis, `biggest_classes[0].holders` must be non-empty. Test does not pass before the fix.

---

## P1 — UI/UX regressions

### P1-1 — Back button broken after internal link navigation

**Symptom:** After clicking a triage jump-link (e.g. "See Leak Suspects"), pressing browser Back or the in-app ← Back button doesn't return to the previous section.

**Root cause (`web/src/App.tsx` ~line 478–490):** `navTo()` uses `history.pushState` correctly, but the custom `NavBreadcrumb` history stack only pops when the *in-app* ← Back is clicked. Browser native Back triggers `popstate`/`hashchange`, which NavBreadcrumb treats as a new forward navigation and pushes instead of pops.

**Fix:** On `popstate`, compare `window.location.hash` against the history stack. If the hash matches a prior entry, pop to that entry. If it's new, push. This makes browser Back and in-app Back behave consistently.

**Files:** `web/src/App.tsx` (NavBreadcrumb component, ~line 485 `useEffect` listening to hashchange/popstate)

**DoD:** Manual test: load report → click triage jump-link → click another section link → press browser Back → confirm returns to previous section.

---

### P1-2 — Self-referential chain classes not discoverable in dominator graph

**Symptom:** For a heap dominated by a deep `Node → Node → Node` chain, the class dominator graph doesn't surface `Node` visually. The "Who Holds This Class?" section also misses it (blocked by P0-2).

**Root cause:** The dominator analysis graph filters out self-referential edges (`dominated_class !== dominator_class`), which hides recursive chains. The `self_referential_classes` field is already populated on the report but isn't displayed in this section.

**Fix:** Add a "Self-Referential Clusters" subsection to the Dominator Analysis section, driven by `report.self_referential_classes`. For each entry show: class name, instance count, total retained, and a visual chain-depth indicator. This surfaces recursive leaks that the class-idom graph otherwise buries.

**Files:** `web/src/App.tsx` (DominatorAnalysis section ~line 4544)

**DoD:** Load the `joint_node_thread_headline_bug` scenario dump; confirm `DeepNode` appears in the Self-Referential Clusters subsection with its chain depth and retained size.

---

## P2 — UX improvements

### P2-1 — Interactive charts: hover tooltips on static charts

**Status:** `ZoomableTreemap` and `PieSection` already have hover. `TreemapBar` tiles have no tooltip. The `TopConsumersTreemap` rects have no tooltip.

**Fix:** Add `title` attribute to `TreemapBar` tiles and `TopConsumersTreemap` rects with `ClassName: retainedSize` content.

**Files:** `web/src/App.tsx`, `web/src/charts.tsx`

**DoD:** Hovering any chart element shows a tooltip with class name and size.

---

### P2-2 — "Analyze (Quick)" should clarify that collection/duplicate detection is disabled

**Status (confirmed by audit):** "Analyze (Full)" already enables `collections=true` and `findDuplicates=true`. The quick path skips these.

**Fix:** Add a tooltip/label to the Quick Analyze button: "Collections and duplicate-string detection are disabled in Quick mode. Use Full Analysis to detect empty ArrayList / over-capacity collections." Alternatively, enable collections in quick mode for small files (< 50 MB), where the cost is negligible.

**Files:** `web/dist/hprof-analyzer-browser.html` (button label ~line 2731)

**DoD:** Quick-analyze the empty-ArrayList demo dump; button tooltip explains why `over-capacity-collections` doesn't fire.

---

## P3 — Test suite expansion

**Architecture:** Two layers:
1. **Synthetic** (`tests/scenarios.rs` + `heap_builder.rs`): fast, no JVM, CI-safe. Covers signal logic.
2. **JVM** (`leak-demos/` + future `tests/jvm_scenarios.rs`): real dumps, covers realism. Already 34 programs; gap is an automated harness that checks report *quality*, not just "doesn't crash."

### P3-1 — Synthetic scenarios: classloader spam, excessive threads, healthy baseline

Existing rules that can be tested synthetically without new rule code:
- `"metaspace-pressure"` — fires when `overview.classes_loaded > 50_000` (MetaspacePressure rule exists)
- `"classloader-explosion"` — fires when `overview.classloaders_loaded >= 1_000` (ClassloaderExplosion exists; needs `classloaders_loaded` to be populated from the dump — requires real ClassLoader instances, hard to fake synthetically)
- `"thread-swarm"` — fires when `threads.threads.len() >= 1_000` (ThreadSwarm exists; needs real Thread instances)

**Feasible synthetic scenarios:**
| # | Name | Signal | How to trigger synthetically |
|---|------|--------|------------------------------|
| 9 | `healthy_baseline` | none (NoneExpected) | Tiny heap with 1 instance of one class, well below all thresholds |
| 10 | `threadlocal_pooled_thread_multiple` | `"threadlocal-leak"` | 5 threads × 1 stale entry each (extends scenario 5 to verify count > 1) |
| 11 | `metaspace_pressure` | `"metaspace-pressure"` | Set up a report with classes_loaded > 50_000 — BUT this requires faking the overview, not actual dump records. May not be feasible without a mock Report path. Check if `analyze_to_report_with_retained` is the only entry point. |

**Scope:** Add scenarios 9–11 if feasible. Update `all_scenarios_have_a_dedicated_test` count.

**Files:** `tests/scenarios.rs`, `tests/common/heap_builder.rs`

**DoD:** `cargo test --test scenarios` passes with updated count.

---

### P3-2 — JVM demo programs: 6 new minimal programs for missing patterns

Add to `leak-demos/` (each ≤ 50 lines, single failure mode, `// expected-triage: <id>` comment):

| File | Pattern | Expected signal |
|------|---------|-----------------|
| `35_ClassLoaderSpam.java` | Repeated URLClassLoader + defineClass in loop | `"metaspace-pressure"` |
| `36_ClassLoaderLeak.java` | URLClassLoader instances held in a static List | `"classloader-explosion"` or `"classloader-leak"` |
| `37_ThreadLocalPooledThread.java` | ExecutorService + ThreadLocal.set() without remove() | `"threadlocal-leak"` |
| `38_ExcessiveThreads.java` | Create and start 10,000 threads | `"thread-swarm"` |
| `39_ThreadExecutorLeak.java` | newFixedThreadPool in a loop, never shutdown | `"thread-swarm"` |
| `40_DirectBufferLeak.java` | ByteBuffer.allocateDirect(10MB) in loop | `"off-heap"` |

Note: `08_ClassLoaderLeak.java`, `06_ExecutorLeak.java`, `13_DirectBufferLeak.java` already exist — new ones should be **simpler** and more targeted.

**Files:** `leak-demos/35_*.java` through `40_*.java`

**DoD:** Each compiles, runs, produces a dump in `leak-demos/dumps/`, and the expected triage signal fires when analyzed by `hprof-analyzer heap report`.

---

### P3-3 — JVM test harness: automated report-quality checks on captured dumps

Create `tests/jvm_scenarios.rs` (gated on `--features jvm_tests` or env `JVM_SCENARIOS_DIR`):
- For each `.java` file in `leak-demos/` that has `// expected-triage: <id>`, load the corresponding `.hprof` from `leak-demos/dumps/`
- Assert the named signal fired
- Assert `primary_culprits[0].class_name` matches `// expected-culprit: <class>` if present

**Files:** `tests/jvm_scenarios.rs` (new), `Cargo.toml` (feature `jvm_tests`)

**DoD:** `JVM_SCENARIOS_DIR=leak-demos/dumps cargo test --features jvm_tests -- jvm_scenarios` passes for all annotated dump files.

---

## P4 — Polish

### P4-1 — Standalone "App Classes" section in nav

A dedicated nav entry pre-filtered to non-JDK classes, always sorted by retained desc. Zero new Rust work — purely a filtered view of `biggest_classes`.

### P4-2 — Recursive cluster depth histogram

For classes in `self_referential_classes`, show a chain-depth histogram using `instance_depth` (already in the model). Surfaces "99% of Node chains are depth 1000+" which is the actionable fact.

### P4-3 — Triage signal → class jump links

When `headline-retainer` names a class, add "▼ Find in Biggest Classes" link that scrolls to + highlights the matching row.

---

## Implementation order

```
P0-1 (thread triple-fire)       → extend scenario 3 + add new scenario
P0-2 (holders window)           → new scenario single_culprit_simple_class
P1-1 (back button)              ← UI only, independent
P1-2 (self-ref in dominator)    ← UI only, independent
P2-1 (chart tooltips)           ← UI only, independent
P2-2 (quick analysis label)     ← UI only, quick
P3-1 (new synthetic scenarios)  → after P0-1 (new rules already verified)
P3-2 (JVM demo programs)        ← independent
P3-3 (JVM harness)              ← after P3-2
P4-*                            ← last
```

# Plan: Holder Breakdown for Top Classes/Objects/Suspects

## Context

When a heap dump shows "HashMap consumed most memory", the report tells you that — but doesn't easily
answer "which classes *hold* all these HashMaps?". The `ImmDomPair` table in Dominator Analysis does
answer it at a class level, but it's in a separate section with no navigation link from the Top
Consumers or Leak Suspects sections where the user first encounters the heavy type. Additionally, in
basic mode (no extra flags), there is no in-section visual showing the 2-level holder breakdown — you
have to search the Dominator Analysis pairs table manually.

This plan adds a **2-level dominator holder breakdown** inline in three sections:
- **Biggest Classes** — per class row: which classes dominate instances of this class (level 1) and
  what dominates *those* (level 2)
- **Biggest Objects** — per object row: its immediate dominator class + that class's immediate dominator
  (i.e. the existing `root_path` first two steps, condensed)
- **Leak Suspects** — for group suspects: the existing `merged_paths` already shows this; for single
  suspects: the existing `root_path` shows it. The gap is just surfacing it better in the render.

The feature is **always-on** (no flag), costs O(top_n × children_per_node) work, and is bounded to
the `top_consumers` cap (default 20 classes / 20 objects) so the 329M-dominator 34GB dump is safe.

---

## Data Model Changes

### 1. `ClassRow` — add `holders` field (`src/report/model.rs:758`)

```rust
pub struct ClassRow {
    pub pretty_class: String,
    pub instances: u64,
    pub retained: u64,
    /// Top immediate-dominator classes for instances of this class.
    /// Each entry: (dominator_class, dominated_instance_count, dominated_retained_sum).
    /// Capped to HOLDER_CAP (10) entries. Empty when class_idx mapping unavailable.
    pub holders: Vec<HolderRow>,
}

pub struct HolderRow {
    pub holder_class: String,
    pub count: u64,
    pub retained: u64,
    /// Top immediate dominators of this holder (level 2). Capped to HOLDER_CAP.
    pub level2: Vec<HolderRow>,
}
```

Both `ClassRow` and `HolderRow` need `serde::Serialize/Deserialize + schemars::JsonSchema` derives.

### 2. `ObjRow` — no struct change needed

The existing `root_path: Option<Vec<RootPathStep>>` in `Suspect` already covers individual objects.
For `ObjRow` (biggest_objects), the first two steps of the single-object root path would be enough —
but `ObjRow` has no path today. Add a lightweight field:

```rust
pub struct ObjRow {
    // ... existing fields ...
    /// First 2 dominator-chain hops toward GC root (class names only, no indices).
    /// [0] = immediate dominator class, [1] = its immediate dominator class.
    /// Empty when object is a direct GC root. Always ≤ 2 entries.
    pub holder_chain: Vec<String>,
}
```

---

## Build Changes (`src/report/build.rs`)

### A. `build_top_consumers` — populate `ClassRow.holders` (near line 4390)

After `class_order` is built and `biggest_classes` rows are being emitted, for each of the top
`top_n` classes, compute the holder breakdown:

```
HOLDER_CAP = 10

For each ci in class_order[0..top_n]:
  Walk ALL top-level dominators (from g.idom[..]): for each obj where class_idx[obj]==ci:
    parent = g.idom[obj]   // immediate dominator
    accumulate (class_idx[parent] → count, retained_sum) into a level-1 map
  Sort level-1 by retained_sum desc, take HOLDER_CAP
  For each level-1 holder class (cid1):
    Walk level-1 holders' own immediate dominators:
      parent2 = g.idom[holder_obj]
      accumulate into level-2 map
    Sort, take HOLDER_CAP → HolderRow.level2
```

**Cost:** This is O(top_n × n) in the worst case if done naively, which is too expensive for top_n=20
on 329M objects. Instead, do it in ONE pass after `class_order` is sorted:

- Collect the **set of top-N class indices** (a `HashSet<u32>` or a `Vec<bool>` flag array of
  size `class_count`).
- In a single O(n) scan over all dense object indices (same domain as `collect_top_level`):
  - For each `obj` where `class_idx[obj]` is in the top-N set:
    - `parent = g.idom[obj]` — look up immediate dominator
    - Update `level1_accum[class_idx[obj]][class_idx[parent]]` += (1, retained[obj])
  - For each `obj` where class_idx[obj] appears as a LEVEL-1 holder of any top-N class AND
    that class also has children in top-N — this is complex, so instead:
    
**Simpler two-pass approach** (still O(n)):
- Pass 1: For each obj, if `class_idx[obj]` ∈ top-N, record `(class_idx[obj], class_idx[idom[obj]])`.
  Accumulate into a `Vec<HashMap<u32, (u64,u64)>>` indexed by top-N class. Size: top_n (20) maps
  with at most O(distinct_dominator_classes) entries each — tiny.
- Pass 2: For each obj, if `class_idx[obj]` appears as a level-1 holder (check a second flag array
  sized class_count, populated from Pass 1 results), record level-2 accumulators.

Both passes are a single O(n) scan over `class_idx`/`idom` arrays which are already in memory.
Total extra RAM: negligible (20 small HashMaps of class indices).

**Where to insert:** After the current `t_tc!("biggest_classes")` call (~line 4402), before `drop(class_retained)`. The `class_idx` and `idom` arrays are still live at this point (via `g`), as is `top_level` (passed in). Iterate over `top_level` (which already contains all top-level dominator indices).

**Important:** `top_level` only covers top-level dominators (direct children of vroot). Objects deeper in the dominator tree are NOT in `top_level`. For classes that appear both as top-level and deeper dominators (common for things like `char[]`), this only covers the top-level slice. That's fine for the use case — the user wants to know "which classes hold the biggest retained contributors".

### B. `build_top_consumers` — populate `ObjRow.holder_chain` (near line 4579)

For each of the top-N biggest objects (`top_level[0..top_n]` sorted by retained):

```rust
let mut chain = Vec::with_capacity(2);
let mut cur = obj;
for _ in 0..2 {
    let par = g.idom[cur];
    if par == vroot || par == UNDEFINED { break; }
    chain.push(display_of(par));
    cur = par;
}
holder_chain: chain,
```

This is O(1) per object (at most 2 `idom` lookups). Already inside the object-row building loop.

---

## Render Changes (`src/report/render_md.rs`)

### A. `render_top_consumers` — Biggest Classes table (near line 1614)

After each class row, if `row.holders` is non-empty, render a collapsible sub-table:

```
**Held by (immediate dominators):**
| Holder class | Count | Retained | Level-2 holders |
|---|---|---|---|
| MyController | 42 | 1.2 GB | → SomeService (12), ... |
| ThreadLocal$... | 7 | 800 MB | → Thread (7) |
```

The level-2 entries are shown inline as a compact `→ Class (count)` list.

### B. `render_top_consumers` — Biggest Objects table (near line 1566)

Add a "Held by" column (always present, shows first 2 chain entries as `A → B`). If `holder_chain`
is empty, show `(GC root)`.

### C. `render_leak_suspects` — no change

The existing `root_path` (for singles) and `merged_paths` (for groups) already provide this data
and are already rendered. No model, build, or render change needed here.

---

## Options Changes (`src/opts.rs`)

Add a `holder_depth` field to `AnalyzeOptions` (default 2, configurable via `--dom-depth N`):

```rust
/// Depth of the inline dominator holder breakdown in Biggest Classes / Biggest Objects.
/// 0 = disabled, 1 = immediate holders only, 2 = holders + their holders (default).
pub holder_depth: u8,
```

DetailLevel presets:
- Minimal: `holder_depth: 1`
- Default: `holder_depth: 2`
- Max: `holder_depth: 2`

Wire `--dom-depth` CLI flag in `src/cli.rs` (or wherever args are parsed).

---

## Critical Files

| File | Change |
|------|--------|
| `src/report/model.rs:758` | Add `HolderRow` struct; add `holders: Vec<HolderRow>` to `ClassRow`; add `holder_chain: Vec<String>` to `ObjRow` |
| `src/report/build.rs:4390` | Populate `ClassRow.holders` after biggest_classes sort, using one O(n) pass over `top_level` |
| `src/report/build.rs:4579` | Populate `ObjRow.holder_chain` with 2 idom lookups per object |
| `src/report/render_md.rs:1558` | Render `holders` sub-table in biggest classes; add "Held by" column to biggest objects |
| `src/opts.rs:109` | Add `holder_depth: u8` to `AnalyzeOptions`; wire `DetailLevel` presets |
| `src/cli.rs` (or arg parser) | Add `--dom-depth N` flag |

---

## Parity / Test Considerations

- `ClassRow` gains a new field — existing golden snapshots will differ if they include the
  `biggest_classes` JSON. Update golden fixtures after implementation.
- `ObjRow` gains `holder_chain` — same fixture impact.
- **Byte-exact output** (the HTML/report content visible to users) is unchanged; we're only adding
  new fields to the JSON model. The rendered tables will change, so parity tests that check HTML
  output will need their golden snapshots regenerated.
- Add a unit test: for a small fixture, assert that the top class's `holders[0].holder_class` is
  the expected class, and `holders[0].level2[0].holder_class` is as expected.

---

## Verification

1. `cargo test --lib` locally (3 cli_repl TTY tests expected-fail, ignore).
2. `cargo test --test parity` — will fail on golden snapshots; regenerate with
   `UPDATE_GOLDEN=1 cargo test --test parity`.
3. Rebuild on ThinkStation, run `--full-analysis` on the 34GB dump, check:
   - `HPROF_TIMING=1` to confirm no regression in `top_consumers` step time.
   - Visually inspect Biggest Classes section: HashMap row should show holder classes.
4. Run `--basic-analysis` (or default `--full-analysis` without extra flags) to confirm the
   feature is present without any extra flag.

# LeakCanary-Inspired Feature Proposals for hprof-analyzer

Inspired by [LeakCanary/Shark](https://github.com/square/leakcanary) and cross-ecosystem
research. Each proposal has a verdict, architecture fit, pros/cons, estimated
size, and implementation sketch against the current codebase.

---

## Table of Contents

| # | Proposal | Verdict |
|---|----------|---------|
| 1 | Category annotation on GC-root path steps | **Accept** |
| 2 | Known-leak pattern catalog via TOML | **Accept** |
| 3 | Leak signatures for dedup / CI tracking | **Accept** |
| 4 | Preserve class hierarchy (superclass chain) | **Accept** |
| 5 | Field-value inspectors for suspect objects | **Explore** |
| 6 | OQL enrichments (multiple sub-proposals) | **Mixed** |

---

## 1. Category Annotation on GC-Root Path Steps (yes please)

### What

Add a `category: String` field to `RootPathStep` so every hop in the
suspect → GC-root chain is labelled `"jdk"`, `"framework"`, `"other"`, or a
user-defined category. Reports and the HTML UI can then colour-code or
group steps, and triage rules can reason about the
application↔framework boundary.

### Verdict: **Accept**

### Pros

- Immediately tells the user *"this step is your code, this step is
  Spring, this step is the JDK"* without manual class-name recognition.
- Enables a follow-on `HeadlineRetainer` rule refinement that prefers
  application-category classes as the "culprit" (proposal 1b below).
- Zero new passes; the `Classifier` is already built and carried on `Graph`.
- Non-breaking schema change (`#[serde(default)]`).
- LeakCanary's design doc calls this the single most important UX
  decision: *"Tell the user which reference is in their code."*

### Cons

- Adds one `String` per path step to the JSON report — negligible size.
- Users who don't configure an `application` category get the default
  `"other"` on all their classes, which is less informative (but no
  worse than today).

### Architecture Fit

`RootPathStep` is defined in `model.rs` (line ~491). The path is built
in `build.rs` by walking `idom[]` upward. The `Classifier` is already
threaded through `build.rs` (used for `HistRow.category`).

### Implementation Sketch

```rust
// model.rs — add one field
pub struct RootPathStep {
    // ... existing fields ...
    #[serde(default)]
    pub category: String,
}

// build.rs — in the loop that builds root_path steps
let category = classifier.classify(&step.display_class).to_string();
steps.push(RootPathStep { /* ... */ category });
```

### Size: ~20 lines

---

### 1b. Application-Class Culprit Heuristic (yes please)

Refine `resolve_primary_culprits()` in `build.rs` to prefer classes in
the `"other"` (or a user-declared `"application"`) category when walking
the suspect path. The current `CulpritRef` logic picks the deepest step
above a retained-% floor that isn't a bare collection internal; this
change would additionally skip `"jdk"` and `"framework"` steps when a
deeper application-class step exists.

**Verdict: Accept** (bundled with proposal 1)

**Size:** ~30 lines in `resolve_primary_culprits`.

---

## 2. Known-Leak Pattern Catalog via TOML      (not now)




### What

A `[[known_leak]]` TOML section that lets users (and shipped defaults)
declare class-name patterns on GC-root paths that represent known
framework bugs or harmless retention. Matched suspects get a label like
*"Library leak: MySQL Connector/J AbandonedConnectionCleanupThread
retains classloader (see JDBC-XXX)"* and are optionally deprioritised in
the triage.

### Verdict: **Accept**

### Pros

- Directly mirrors LeakCanary's `AndroidReferenceMatchers` — a curated
  database of known framework leaks, each with a description and
  optional version gate.
- Noise reduction: framework leaks that the user can't fix stop
  drowning out application leaks.
- Extensible by users via `.hprof-analyzer.toml` without recompilation.
- Can ship with a default catalog covering the most common JDK /
  framework patterns (MySQL JDBC driver classloader leak, Logback
  `Logger.parent`, Tomcat `WebappClassLoader`, Spring DevTools
  `RestartClassLoader`, Log4j plugin registry, etc.).
- The `Rule` trait in `triage.rs` is the natural hook — a single
  `KnownLeakAnnotator` rule that iterates suspects' `root_path` and
  pattern-matches.

### Cons

- Pattern language design requires thought: exact class sequence?
  Substring? Field name? Regex? Over-engineering the matcher DSL could
  stall implementation.
- Maintaining a default catalog is ongoing work (each framework version
  can change retention paths).
- Risk of false suppression: an overly broad pattern could hide a real
  application leak behind a "known library leak" label.

### Architecture Fit

The TOML config machinery in `collection_config.rs` already parses
`[[collection]]` and `[[category]]` sections. Adding `[[known_leak]]` is
structurally identical: a new `#[derive(Deserialize)] struct
RawKnownLeak` and a parsing step.

The `Rule` trait gives a clean hook: the rule receives `&Report` (which
contains `Suspect.root_path`) and can emit a `TriageSignal` with
severity `Info` and a descriptive title.

### Implementation Sketch

```toml
# .hprof-analyzer.toml
[[known_leak]]
description = "MySQL JDBC AbandonedConnectionCleanupThread classloader leak"
path_contains = ["com.mysql.cj.jdbc.AbandonedConnectionCleanupThread"]
severity = "info"      # info | warning | suppress
reference = "https://bugs.mysql.com/bug.php?id=..."

[[known_leak]]
description = "Spring DevTools RestartClassLoader retained after reload"
path_contains = ["org.springframework.boot.devtools.restart.RestartClassLoader"]
severity = "info"
```

```rust
// triage.rs
struct KnownLeakAnnotator { patterns: Vec<KnownLeakPattern> }

impl Rule for KnownLeakAnnotator {
    fn eval(&self, r: &Report) -> Option<TriageSignal> {
        for suspect in &r.leaks.suspects {
            if let Some(path) = &suspect.root_path {
                let classes: Vec<&str> = path.iter()
                    .map(|s| s.display_class.as_str()).collect();
                for pat in &self.patterns {
                    if pat.matches(&classes) {
                        return Some(signal(
                            "known-leak", Info,
                            &format!("Known library leak: {}", pat.description),
                            &pat.detail_with_reference(),
                            None,
                        ));
                    }
                }
            }
        }
        None
    }
}
```

### Size: ~200 lines (config parsing + rule + default catalog file)

### Open Questions

1. **Pattern language**: Start with `path_contains` (any step's class
   matches a substring) and `path_sequence` (ordered subsequence of
   class names). Skip regex for v1.
2. **Per-suspect vs per-report**: Should the rule annotate individual
   suspects (adding a `known_leak_label: Option<String>` to `Suspect`)
   or emit a single triage signal? Both are useful; start with triage
   signal, add per-suspect label later.
3. **Default catalog**: Ship a `known-leaks.toml` embedded in the binary
   (via `include_str!`) with ~20 patterns for the most common
   JDK/Spring/Hibernate/JDBC/Tomcat leaks. Users can override or extend.

---

## 3. Leak Signatures for Dedup / CI Tracking (not now)

### What

Compute a stable hash ("leak signature") for each suspect based on its
GC-root path's class+field sequence. Two suspects with the same
signature represent the same bug, even across different heap dumps or
machines.

### Verdict: **Accept**

### Pros

- Enables deduplication in `compare reports`: "suspect X appeared in all
  5 dumps with the same signature → confirmed persistent leak."
- Enables CI integration: a test can assert "no new leak signatures
  since baseline."
- Matches LeakCanary's design: SHA-1 of the concatenated suspect
  reference identifiers. LeakCanary uses this as `leak.signature` in
  Bugsnag/crash-tracker uploads.
- Non-breaking schema change (new optional field on `Suspect`).

### Cons

- Signature stability depends on path stability: if the dominator tree
  changes slightly between runs (due to GC timing, different object
  addresses), the path may change, producing a different signature for
  the same bug.
- Without field names (`--ref-paths`), the signature is class-only,
  which is coarser (two different leaks through the same class sequence
  would collide).
- Users may misunderstand: same signature ≠ same root cause (it means
  same *path pattern*, which is a strong but not perfect signal).

### Architecture Fit

`Suspect.root_path` is `Option<Vec<RootPathStep>>`. Each step has
`display_class: String` and `field_edge: Option<String>`. Computing
SHA-256 (or even a simpler hash) over the concatenated
`"class.field|class.field|..."` string is trivial.

### Implementation Sketch

```rust
// model.rs — add to Suspect
#[serde(default, skip_serializing_if = "Option::is_none")]
pub signature: Option<String>,

// build.rs — after building root_path
use sha2::{Sha256, Digest};
let sig = if let Some(path) = &suspect.root_path {
    let input: String = path.iter().map(|s| {
        match &s.field_edge {
            Some(f) => format!("{}.{}", s.display_class, f),
            None    => s.display_class.clone(),
        }
    }).collect::<Vec<_>>().join("|");
    Some(format!("{:x}", Sha256::digest(input.as_bytes())))
} else { None };
```

### Size: ~40 lines + `sha2` crate dependency (already used? if not, use `std` hashing)

### Follow-on: `compare reports` Enhancement

In `diff_reports.rs`, `SeriesSuspectRow` can be extended with
`signature: Option<String>` and a `signature_stable_across_all: bool`
flag. A new triage signal `ConfirmedPersistentLeak` fires when a
signature appears in every report in the series.

**Size:** ~50 additional lines.

---

## 4. Preserve Class Hierarchy (Superclass Chain)    (implement without interface support)

### What

Capture and retain the superclass relationship from HPROF `CLASS_DUMP`
records, making it available as a `class_superclass_idx: Vec<u32>` array
on `Graph`. This enables queries like "is this a `Thread` subclass?" or
"does this implement `Closeable`?" and unlocks richer structural
inspectors and OQL functions.

### Verdict: **Accept**

### Pros

- Unlocks `INSTANCEOF` correctness in OQL: currently `FROM INSTANCEOF
  X` works by checking the class hierarchy, but if the hierarchy isn't
  fully preserved, deep subclass chains may be missed.
- Enables structural inspectors: "all `java.lang.Thread` subclasses
  retaining > 10 MB" without hard-coding class names.
- Enables a future `@superclass` OQL attribute and `IMPLEMENTS`
  keyword.
- The data is already in the HPROF file — `CLASS_DUMP` records carry
  `super_class_object_id`. Pass1 already processes these records;
  it just doesn't forward the superclass ID.
- Small memory cost: one `u32` per class (~4 bytes × class count; even
  200k classes = 800 KB).

### Cons

- Pass1 change: needs to store and forward the superclass serial/index
  alongside class name and loader ID.
- Interface information is NOT in HPROF: `CLASS_DUMP` only has
  `super_class_object_id`, not implemented interfaces. So
  "implements `Closeable`" remains impossible from a heap dump alone.
  Only "extends `Thread`" style queries work.
- Minor schema version bump (new data on `Graph`, not on `Report`
  directly — but if exposed in OQL or report fields, affects schema).

### Architecture Fit

Pass1 (`pass1.rs`) processes `CLASS_DUMP` records and builds the class
table. The superclass object ID is read but currently used only to
compute field layout (for shallow size calculation via
`calculateSizeRecursive`). Preserving it as an index into the class
table requires resolving the superclass object ID to a class index
during or after pass1 — same lookup pattern already used for
`class_obj_class_idx`.

### Implementation Sketch

```rust
// In pass1 state — add alongside class_names, class_loader_id, etc.
pub class_super_idx: Vec<u32>,  // super_idx[c] = class-table index of c's superclass
                                 // u32::MAX = no superclass (java.lang.Object)

// During CLASS_DUMP processing:
let super_class_id = reader.read_id();
// After pass1, resolve: map super_class_id → class index via id_map lookup
for c in 0..class_count {
    let super_obj_id = raw_super_ids[c];
    class_super_idx[c] = if super_obj_id == 0 {
        u32::MAX
    } else {
        id_map.lookup(super_obj_id).unwrap_or(u32::MAX)
    };
}
```

### Size: ~100 lines in pass1 + ~50 lines for OQL `@superclass` attribute + ~50 for `INSTANCEOF` correctness fix

### Open Questions

1. **Timing**: The superclass object ID needs to be resolved to a class
   index *after* all `CLASS_DUMP` records are parsed (since forward
   references are possible). This fits naturally into the pass1 → pass2
   transition.
2. **Interface support**: HPROF doesn't carry interface information.
   If interface queries are needed, they'd require heuristic matching
   (e.g., checking if a class has a field pattern consistent with
   implementing `Closeable`) or external metadata. Not worth it for v1.

---

## 5. Field-Value Inspectors for Suspect Objects (not now)

### What

Read actual field values (like `Session.closed`, `Thread.state`,
`ViewModel.mCleared`) from suspect objects to annotate them with
lifecycle status — the JVM equivalent of LeakCanary's
`ObjectInspector` system that produces "Leaking: YES / NO / UNKNOWN"
verdicts.

### Verdict: **Explore** (high value but significant architecture work)

### Pros

- This is LeakCanary's single most impactful feature. Reading
  `Activity.mDestroyed = true` transforms a generic "this object is
  retained" signal into "this object should have been garbage collected."
- For server-side Java: reading `Session.isOpen()` (via the `open`
  field), `ExecutorService.isShutdown()` (via internal state fields),
  `Connection.isClosed()` would be transformative.
- The `objread.rs` module already supports random-access field reads
  for OQL queries — the plumbing exists.
- Natural extension of the `Rule` trait: an inspector rule that receives
  the suspect's object index and uses `objread` to decode specific
  fields.

### Cons

- **Architecture mismatch**: The `Rule` trait operates on `&Report` (the
  finished model), which has no access to `Graph` or the HPROF file.
  Inspectors that read fields need `Graph` + the HPROF reader, which
  are dropped before triage runs.
- **Two options, both costly**:
  1. *Run inspectors during `build.rs`* while `Graph` is live. This
     requires a new trait (`ObjectInspector`) and a callback mechanism
     in the report-building pipeline. ~500+ lines.
  2. *Re-open the HPROF for suspects only* after report building. This
     is simpler (~300 lines) but adds I/O and requires keeping the file
     path around.
- **Field name knowledge**: Inspectors need to know the internal field
  name (e.g., `mDestroyed` for Android `Activity`, `closed` for JDBC
  `Connection`). These are not standardised across framework versions.
  A config-driven approach (TOML defining class + field + expected value
  → verdict) is more maintainable than hard-coded checks.
- **False verdicts**: Reading a field value at dump time may not reflect
  the logical state (e.g., a `Connection` object may have `closed=false`
  but be in a broken state). Conservative defaults are essential.

### Architecture Fit

The cleanest approach is option (2): a post-report "inspector pass" that
re-opens the HPROF file, seeks to each suspect's object record via
`objread`, decodes specific fields, and annotates the `Suspect` struct.
The `objread` module already has `read_instance_fields()` which returns
field name-value pairs.

### Implementation Sketch (option 2)

```rust
// New: src/inspect.rs
pub struct FieldInspector {
    pub class_prefix: String,
    pub field_name: String,
    pub leaking_value: FieldValue, // e.g., Bool(true), Null
    pub leaking_reason: String,
    pub not_leaking_reason: String,
}

pub fn inspect_suspects(
    hprof_path: &Path,
    suspects: &mut [Suspect],
    inspectors: &[FieldInspector],
    graph: &Graph,  // or: index data needed for seek
) -> io::Result<()> {
    for suspect in suspects.iter_mut() {
        let obj_idx = suspect.obj_index_1based - 1;
        let class_name = &suspect.pretty_class;
        for insp in inspectors {
            if class_name.starts_with(&insp.class_prefix) {
                let fields = objread::read_fields(hprof_path, obj_idx, graph)?;
                if let Some(val) = fields.get(&insp.field_name) {
                    if *val == insp.leaking_value {
                        suspect.verdict = Some("leaking".into());
                        suspect.verdict_reason = Some(insp.leaking_reason.clone());
                    }
                }
            }
        }
    }
    Ok(())
}
```

```toml
# .hprof-analyzer.toml — declarative inspectors
[[inspector]]
class = "java.sql.Connection"
field = "closed"
leaking_when = true
leaking_reason = "Connection is closed but still retained"
not_leaking_reason = "Connection is open (normal)"

[[inspector]]
class = "org.hibernate.internal.SessionImpl"
field = "closed"
leaking_when = true
leaking_reason = "Hibernate Session closed but still retained"
```

### Size: ~500 lines (inspector framework + TOML config + a few built-in inspectors)

### Open Questions

1. **Should this block the other proposals?** No. Proposals 1–4 are
   independent and should ship first.
2. **What about the `Rule` trait?** A `Rule` implementation could check
   the `verdict` field *after* the inspector pass populates it —
   keeping the two systems composable.
3. **Memory cost**: Opening the HPROF for random reads on ~10 suspect
   objects is negligible (<1ms per object).

---

## 6. OQL Enrichments

### Overview

Multiple sub-proposals, each independent. Organised by verdict.

---

### 6a. `@category` Virtual Attribute  (yes please)

**What**: Expose the `Classifier` result as `@category` in OQL, so
users can write `WHERE @category = "framework"` or
`GROUP BY @category`.

**Verdict: Accept**

**Pros**: Zero-cost lookup (classifier already on `Graph`); immediately
useful for filtering and grouping.

**Cons**: Requires threading the `Classifier` into the OQL evaluation
context. Minor plumbing.

```sql
SELECT @displayName, @retainedHeapSize, @category
FROM INSTANCEOF java.lang.Object
WHERE @category = "application"
ORDER BY @retainedHeapSize DESC LIMIT 20
```

**Size**: ~40 lines (add to attribute resolution in `query/eval.rs`).

---

### 6b. `@dominatorClass` and `@dominatorDepth` Virtual Attributes (yes please)

**What**: Expose the immediate dominator's class name and the object's
depth in the dominator tree as queryable attributes.

**Verdict: Accept**

**Pros**: Enables "find all byte[] dominated by HashMap" or "find
objects deeper than 50 in the dominator tree" without walking the tree
manually. The data is already computed (`idom[]` + `depth_counts`).

**Cons**: `@dominatorDepth` would need per-object depth, which is
currently only aggregated as a histogram. Computing per-object depth
adds one `u32` per object (~40 MB for 10M objects). Alternatively,
compute on-the-fly during query evaluation (walk `idom[]` upward,
count steps — O(depth) per object, fast for shallow objects but
O(n) worst case for degenerate chains).

```sql
-- What classes dominate the most byte[] arrays?
SELECT @dominatorClass, COUNT(*) AS n, SUM(@usedHeapSize) AS total
FROM byte[]
GROUP BY @dominatorClass
ORDER BY total DESC LIMIT 20

-- Objects very deep in the dominator tree (possible linked-list leak)
SELECT @displayName, @dominatorDepth, @retainedHeapSize
FROM INSTANCEOF java.lang.Object
WHERE @dominatorDepth > 100
ORDER BY @retainedHeapSize DESC LIMIT 20
```

**Size**: ~60 lines.

---

### 6c. `@superclass` Virtual Attribute (yes please)

**What**: Expose the superclass name, dependent on proposal 4.

**Verdict: Accept** (contingent on proposal 4)

```sql
-- All subclasses of AbstractEntity and their total retained size
SELECT @displayName, @superclass, @retainedHeapSize
FROM INSTANCEOF java.lang.Object
WHERE @superclass = "com.example.AbstractEntity"
ORDER BY @retainedHeapSize DESC
```

**Size**: ~20 lines (once hierarchy is on `Graph`).

---

### 6d. `inbounds()` and `outbounds()` Functions (just `outbounds()` for now)

**What**: Reference-navigation functions returning the set of objects
that reference (or are referenced by) a given object.

**Verdict: Defer**

**Pros**: MAT's most-used OQL functions after `toString()`. Essential
for "who points to this object?" queries.

**Cons**: `inbounds()` requires the **inbound CSR** (predecessor graph),
which is built during pass2 for dominator computation but then
compressed and partially dropped for memory efficiency. Keeping the full
inbound CSR accessible for OQL queries would add significant RSS
(~8 bytes × edge count; 30M edges = 240 MB). `outbounds()` requires
the forward CSR, which is available when `--ref-paths` is set.

A compromise: implement `outbounds()` (forward CSR, already available)
first, defer `inbounds()` to a future "query-mode" flag that preserves
the inbound CSR.

**Size**: ~150 lines for `outbounds()`; ~300 for `inbounds()` with CSR
retention.

---

### 6e. `dominators()` and `dominatorof()` Functions (yes please)

**What**: Dominator-tree navigation functions.

**Verdict: Accept**

**Pros**: `dominatorof(obj)` is a simple `idom[]` lookup — already on
`Graph`. `dominators(obj)` (children in the dominator tree) requires
the children CSR from `retained.rs::build_dom_children_csr()`, which
is currently built and then dropped. Keeping it is cheap (one `Vec<u32>`
offset array + one `Vec<u32>` children array).

```sql
-- Top dominated children of the biggest suspect
SELECT @displayName, @retainedHeapSize
FROM OBJECTS dominators(0x1a2b3c4d)
ORDER BY @retainedHeapSize DESC LIMIT 20

-- Walk up the dominator chain
SELECT dominatorof(dominatorof(o)) FROM OBJECTS 0x1a2b3c4d o
```

**Size**: ~80 lines (retain children CSR + expose two functions).

---

### 6f. `classof()` Function

**What**: Return the class object for an instance (or the class name as
a string).

**Verdict: Accept**

**Pros**: Trivial — `class_idx[]` → `class_names[]` lookup. Useful for
grouping: `GROUP BY classof(referrer)`.

```sql
SELECT classof(o) AS class, COUNT(*) AS n
FROM INSTANCEOF java.lang.Object o
GROUP BY classof(o)
ORDER BY n DESC LIMIT 20
```

**Size**: ~20 lines.

---

### 6g. Collection-Aware Queries (Virtual Table Projection) (yes please)

**What**: Treat `HashMap` as a queryable key-value table,
`ArrayList` as a single-column table, `ThreadLocal` entries as
thread-attributed rows.

**Verdict: Defer**

**Pros**: Extremely powerful for interactive exploration. LeakCanary's
`ReferenceReader` (virtual references) and MAT's collection extractors
both aim at this. jafar's HdumpPath also supports this with pipeline
syntax.

**Cons**: Requires per-collection-type "extractor" logic that decodes
the internal structure (bucket arrays, linked lists, tree nodes).
hprof-analyzer already has this for the `--collections` analysis
(collection fill ratios, size distributions), but it's not wired into
the OQL engine. Bridging the two is non-trivial (~500+ lines).

```sql
-- Hypothetical: treat HashMap as key-value table
SELECT key, value, @retainedHeapSize
FROM ENTRIES java.util.HashMap m
WHERE m.@retainedHeapSize > 1048576
ORDER BY @retainedHeapSize DESC LIMIT 20

-- Hypothetical: ThreadLocal values by thread
SELECT thread, key, value
FROM THREADLOCALS
WHERE thread LIKE "%http-nio%"
```

**Size**: ~500+ lines. Worth doing eventually but not in the first
batch.

---

### 6h. `pathToRoot()` Function (yes please)

**What**: Return the dominator chain from an object to its GC root as a
queryable result set.

**Verdict: Accept**

**Pros**: The implementation already exists — `build.rs` walks `idom[]`
upward to produce `Vec<RootPathStep>`. Exposing this as an OQL function
is a thin wrapper. This is VisualVM's `heap.livepaths()` equivalent.

```sql
-- Show the retention chain for the largest HashMap
SELECT pathToRoot(m)
FROM java.util.HashMap m
ORDER BY @retainedHeapSize DESC LIMIT 1
```

**Size**: ~60 lines (wrap existing `idom[]` walk as a function, format
the result as a multi-row output).

---

### 6i. `LIKE` with SQL Wildcards in WHERE  (yes please)

**What**: `WHERE toString(s) LIKE "%admin%"` using SQL `%`/`_`
wildcards (in addition to the existing regex `LIKE "regex"` syntax).

**Verdict: Accept** (if not already supported)

**Pros**: More familiar to SQL users than regex. MAT supports both.

**Cons**: Minor parser change to distinguish SQL LIKE (`%`) from regex.

**Size**: ~30 lines.

---

### 6j. `UNION` of Queries (yes please)

**What**: Combine results from multiple `SELECT` statements.

**Verdict: Accept**

**Pros**: Standard SQL feature. Useful for combining results across
different class hierarchies.

**Cons**: Parser extension. Need to handle column-type compatibility.

```sql
SELECT toString(s) AS value, @usedHeapSize FROM java.lang.String s WHERE @usedHeapSize > 10000
UNION
SELECT toString(s) AS value, @usedHeapSize FROM java.lang.StringBuilder s WHERE @usedHeapSize > 10000
```

**Size**: ~80 lines (parser + executor).

---

## Summary: Priority Order

| Priority | Proposal | Verdict | Size |
|----------|----------|---------|------|
| **P0** | 1. Category on path steps | Accept | ~20 loc |
| **P0** | 1b. Application-class culprit | Accept | ~30 loc |
| **P0** | 3. Leak signatures | Accept | ~40 loc |
| **P1** | 4. Class hierarchy | Accept | ~200 loc |
| **P1** | 2. Known-leak catalog | Accept | ~200 loc |
| **P1** | 6a. `@category` in OQL | Accept | ~40 loc |
| **P1** | 6b. `@dominatorClass`/`@dominatorDepth` | Accept | ~60 loc |
| **P1** | 6e. `dominatorof()`/`dominators()` | Accept | ~80 loc |
| **P1** | 6f. `classof()` | Accept | ~20 loc |
| **P1** | 6h. `pathToRoot()` | Accept | ~60 loc |
| **P2** | 6c. `@superclass` | Accept | ~20 loc |
| **P2** | 6i. `LIKE` with SQL wildcards | Accept | ~30 loc |
| **P2** | 6j. `UNION` | Accept | ~80 loc |
| **P3** | 5. Field-value inspectors | Explore | ~500 loc |
| **P3** | 6d. `inbounds()`/`outbounds()` | Defer | ~300 loc |
| **P3** | 6g. Collection-aware queries | Defer | ~500 loc |

**P0** items are zero-risk, high-value, and can ship independently.
**P1** items require moderate design but have clear architecture fits.
**P2** items depend on P1 work (hierarchy) or are smaller quality-of-life.
**P3** items are high-value but need architecture exploration first.

---

## Sources

- [LeakCanary design decisions](https://square.github.io/leakcanary/design/)
- [LeakCanary Shark architecture](https://square.github.io/leakcanary/shark/)
- [LeakCanary ObjectInspector recipes](https://square.github.io/leakcanary/recipes/)
- [LeakCanary AndroidReferenceMatchers](https://github.com/square/leakcanary/blob/main/shark/shark-android/src/main/java/shark/AndroidReferenceMatchers.kt)
- [LeakCanary PrioritizingShortestPathFinder](https://github.com/square/leakcanary/blob/0f9b924b/shark/shark/src/main/java/shark/PrioritizingShortestPathFinder.kt)
- [LeakCanary heap growth detection](https://github.com/square/leakcanary/blob/main/docs/heap-growth.md)
- [Eclipse MAT OQL syntax](https://help.eclipse.org/latest/topic/org.eclipse.mat.ui.help/reference/oqlsyntax.html)
- [Eclipse MAT property accessors](https://help.eclipse.org/latest/topic/org.eclipse.mat.ui.help/reference/propertyaccessors.html)
- [VisualVM OQL help](https://robberphex.github.io/visualvm.github.io/oqlhelp.html)
- [jafar HdumpPath reference](https://github.com/btraceio/jafar/blob/main/doc/hdumppath.md)
- [JOL heapdump-duplicates](https://github.com/openjdk/jol)
- [Perfetto SQL heap analysis](https://perfetto.dev/docs/analysis/builtin)
- [Spencer: Interactive Heap Analysis (MSR 2017)](https://arxiv.org/html/1703.05615v2)

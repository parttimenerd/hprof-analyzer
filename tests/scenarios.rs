//! Synthetic-scenario regression tests — the plan's "primary deliverable":
//! a `Scenario` table + runner where adding a new test case is "append one
//! row," checked against `primary_culprits` / `self_referential_classes` /
//! `triage` rather than just "the tool didn't crash."
//!
//! Scope deliberately limited to the 3 scenarios the plan called out by
//! name for the first pass (self-referential tree, empty-oversized
//! ArrayList, and the joint Node/Thread headline bug) — the builder
//! primitives in `tests/common/heap_builder.rs` exist only to serve these;
//! extend both together as new scenarios are added, not ahead of need.
//!
//! Dumps are generated in-memory (no JVM, no temp files, sub-millisecond)
//! and analyzed in-process via `hprof_analyzer::analyze_to_report_with_retained`
//! — no subprocess spawn, so failures give a real Rust backtrace instead of
//! opaque stdout/stderr scraping.
//!
//! ## Adding a new scenario
//! 1. Add any new builder primitive you need to `tests/common/heap_builder.rs`
//!    (only what's needed — reuse `HeapBuilder::class`/`instance`/`obj_array`/
//!    `class_with_static_root` first; most shapes don't need a new primitive).
//! 2. Write a `build_<name>() -> Vec<u8>` function constructing the dump.
//! 3. Write a `check_<name>(report: &Report)` function asserting the
//!    must-fire/must-not-fire signal ids, `must-name` culprit (via
//!    `assert_culprit`), and/or any other `Report` field relevant to the
//!    scenario.
//! 4. Add a `Scenario { .. }` row to the `SCENARIOS` table and a dedicated
//!    `#[test] fn scenario_<name>() { run_scenario(&SCENARIOS[N]); }`.
//! 5. Bump the count in `all_scenarios_have_a_dedicated_test`.

#[path = "common/heap_builder.rs"]
mod heap_builder;

use heap_builder::HeapBuilder;
use hprof_analyzer::report::Report;
use hprof_analyzer::{AnalyzeOptions, HprofSource};

// ---------------------------------------------------------------------------
// Assertion contract
// ---------------------------------------------------------------------------

/// What a scenario's analysis is expected to conclude about "who's
/// responsible." Checked against `Report.primary_culprits` — the resolved,
/// de-duplicated answer every headline/section is supposed to agree on
/// (see `report::build::resolve_primary_culprits`).
enum CulpritExpectation {
    /// Exactly one dominant culprit, whose resolved `class_name` must
    /// contain `class_contains` (a substring match — display names are
    /// dotted, e.g. `"TreeNode"` matches `"scenarios.TreeNode"`).
    Named { class_contains: &'static str },
    /// No suspect should cross the dominance floor — a healthy/diffuse
    /// heap must not be forced into naming a culprit it doesn't have.
    /// (Not exercised by the first 3 scenarios; included so the enum's
    /// shape doesn't need to change when a healthy-baseline scenario is
    /// added later, per the plan's P2 item 11.)
    #[allow(dead_code)]
    NoneExpected,
}

/// One test scenario: a dump-producing closure plus the assertions it must
/// satisfy. Deliberately NOT generic/data-driven beyond this — each
/// scenario's `check` closure can inspect whatever `Report` fields are
/// relevant (triage signal ids, self_referential_classes, biggest_classes,
/// ...), since "must-fire signal X" and "must-name class Y" are different
/// shapes of assertion that don't compress well into one declarative table
/// without losing readability (confirmed while drafting: a fully generic
/// table variant needed as much boilerplate per-row as a closure, for less
/// clarity at the call site).
struct Scenario {
    name: &'static str,
    /// AnalyzeOptions tweak (e.g. `collections = true`); defaults otherwise.
    opts: fn() -> AnalyzeOptions,
    build: fn() -> Vec<u8>,
    check: fn(&Report),
}

fn run_scenario(s: &Scenario) {
    let bytes = (s.build)();
    let source = HprofSource::from_bytes(bytes, s.name);
    let opts = (s.opts)();
    let (report, _retained) = hprof_analyzer::analyze_to_report_with_retained(&source, &opts)
        .unwrap_or_else(|e| {
            panic!("scenario {:?}: analysis failed: {e}", s.name);
        });
    (s.check)(&report);
}

/// Assert `report.primary_culprits` matches `expect`. Centralizing this
/// (rather than inlining in every scenario's `check`) is what makes the
/// "must-name-the-class" contract uniform across scenarios instead of each
/// one re-deriving its own ad hoc string match.
fn assert_culprit(report: &Report, expect: &CulpritExpectation) {
    match expect {
        CulpritExpectation::Named { class_contains } => {
            assert!(
                !report.primary_culprits.is_empty(),
                "expected a dominant culprit naming {class_contains:?}, but \
                 primary_culprits is empty\nleaks.suspects: {:#?}",
                report.leaks.suspects,
            );
            let got = &report.primary_culprits[0].class_name;
            assert!(
                got.contains(class_contains),
                "primary_culprits[0].class_name = {got:?}, expected it to contain \
                 {class_contains:?}\nfull primary_culprits: {:#?}",
                report.primary_culprits,
            );
        }
        CulpritExpectation::NoneExpected => {
            assert!(
                report.primary_culprits.is_empty(),
                "expected no dominant culprit (diffuse/healthy heap), but got: {:#?}",
                report.primary_culprits,
            );
        }
    }
}

/// Assert a triage signal with the given stable id fired (`must-fire`).
fn assert_signal_fires(report: &Report, id: &str) {
    assert!(
        report.triage.iter().any(|s| s.id == id),
        "expected triage signal {id:?} to fire; got ids: {:?}",
        report.triage.iter().map(|s| &s.id).collect::<Vec<_>>(),
    );
}

/// Assert a triage signal with the given stable id did NOT fire
/// (`must-not-fire`, the false-positive guard).
#[allow(dead_code)]
fn assert_signal_silent(report: &Report, id: &str) {
    assert!(
        !report.triage.iter().any(|s| s.id == id),
        "expected triage signal {id:?} to stay silent, but it fired",
    );
}

// ---------------------------------------------------------------------------
// Scenario 1: self-referential tree (SelfReferentialClassAccumulation)
// ---------------------------------------------------------------------------

fn build_self_referential() -> Vec<u8> {
    let mut b = HeapBuilder::new();
    // 50 nodes, each padded to a few hundred bytes of shallow size via inline
    // int fields, so the chain's total retained comfortably clears
    // SELF_REF_MIN_RETAINED (64 KiB) and SELF_REF_MIN_INSTANCES (10) with
    // margin (a prior 40-node/400-int version landed at 64960 B, just under
    // the 65536 B floor).
    heap_builder::build_self_referential_tree(&mut b, "scenarios/TreeNode", 50, 400);
    b.build()
}

fn check_self_referential(report: &Report) {
    assert_signal_fires(report, "self-referential-class-accumulation");
    assert!(
        !report.self_referential_classes.is_empty(),
        "expected self_referential_classes to be populated"
    );
    let row = &report.self_referential_classes[0];
    assert!(
        row.pretty_class.contains("TreeNode"),
        "self_referential_classes[0].pretty_class = {:?}, expected it to name TreeNode",
        row.pretty_class,
    );
    assert_culprit(
        report,
        &CulpritExpectation::Named {
            class_contains: "TreeNode",
        },
    );
}

// ---------------------------------------------------------------------------
// Scenario 2: empty oversized ArrayList (OverCapacityCollections)
// ---------------------------------------------------------------------------

fn build_empty_oversized_list() -> Vec<u8> {
    let mut b = HeapBuilder::new();
    // Capacity 200_000, zero live elements: ~1.6 MB of wasted backing-array
    // capacity on an otherwise near-empty heap, so the wasted-% floor clears
    // easily regardless of exact overhead accounting elsewhere.
    heap_builder::build_empty_oversized_arraylist(&mut b, 200_000);
    b.build()
}

fn opts_with_collections() -> AnalyzeOptions {
    let mut o = AnalyzeOptions::default();
    o.collections = true;
    o
}

fn check_empty_oversized_list(report: &Report) {
    assert_signal_fires(report, "over-capacity-collections");
    assert!(
        report.collections.collection_fill_ratio.tracked > 0,
        "expected at least one tracked collection"
    );
}

// ---------------------------------------------------------------------------
// Scenario 3: joint Node/Thread headline bug — the deep tree must be named,
// not the nearest GC-root holder (Thread) that merely transits it.
// ---------------------------------------------------------------------------

fn build_joint_node_thread() -> Vec<u8> {
    let mut b = HeapBuilder::new();
    heap_builder::build_joint_node_thread_scenario(&mut b, "scenarios/DeepNode", 60, 500);
    b.build()
}

fn check_joint_node_thread(report: &Report) {
    // The headline must name the deep tree class, NOT "Thread" — this is
    // the exact regression `primary_culprits` (build.rs commit 680d70c) was
    // introduced to fix: a top-level GC-root holder wrongly promoted over
    // the real, deeper culprit.
    assert_culprit(
        report,
        &CulpritExpectation::Named {
            class_contains: "DeepNode",
        },
    );
    let culprit = &report.primary_culprits[0];
    assert_ne!(
        culprit.class_name, "java.lang.Thread",
        "primary culprit regressed to naming the transiting Thread GC-root \
         holder instead of the real deep-tree culprit"
    );

    // DominantGcRootType ("gc-root-type") must not independently re-surface
    // "Thread" as a second, redundant headline now that primary_culprits
    // already resolved the real (deeper) culprit — this is exactly the
    // "three redundant headlines" complaint the primary_culprits refinement
    // (build.rs commit 680d70c) fixed via suspect-class suppression.
    for sig in &report.triage {
        if sig.id == "gc-root-type" {
            panic!(
                "DominantGcRootType re-surfaced as a second headline despite \
                 primary_culprits already naming {:?}: {}",
                culprit.class_name, sig.detail,
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Scenario 4: Off-heap DirectByteBuffer (fires "off-heap" triage signal)
// ---------------------------------------------------------------------------

fn build_off_heap_direct_byte_buffer() -> Vec<u8> {
    let mut b = HeapBuilder::new();
    heap_builder::build_off_heap_direct_byte_buffer(&mut b);
    b.build()
}

fn check_off_heap_direct_byte_buffer(report: &Report) {
    assert_signal_fires(report, "off-heap");
}

// ---------------------------------------------------------------------------
// Scenario 5: ThreadLocal null-key leak (fires "threadlocal-leak" signal)
// ---------------------------------------------------------------------------

fn build_threadlocal_null_key_leak() -> Vec<u8> {
    let mut b = HeapBuilder::new();
    heap_builder::build_threadlocal_null_key_leak(&mut b);
    b.build()
}

fn check_threadlocal_null_key_leak(report: &Report) {
    assert_signal_fires(report, "threadlocal-leak");
    assert!(
        report.leak_indicators.thread_local_null_key_count > 0,
        "expected thread_local_null_key_count > 0"
    );
}

// ---------------------------------------------------------------------------
// Scenario 6: Boxed-primitive bloat (fires "boxed-primitive-bloat" signal)
// ---------------------------------------------------------------------------

fn build_boxed_primitive_bloat() -> Vec<u8> {
    let mut b = HeapBuilder::new();
    heap_builder::build_boxed_primitive_bloat(&mut b);
    b.build()
}

fn check_boxed_primitive_bloat(report: &Report) {
    assert_signal_fires(report, "boxed-primitive-bloat");
}

// ---------------------------------------------------------------------------
// Scenario 7: Duplicate strings (fires "duplicate-strings" signal)
// ---------------------------------------------------------------------------

fn build_duplicate_strings() -> Vec<u8> {
    let mut b = HeapBuilder::new();
    heap_builder::build_duplicate_strings(&mut b);
    b.build()
}

fn opts_with_find_duplicates() -> AnalyzeOptions {
    let mut o = AnalyzeOptions::default();
    o.find_duplicates = true;
    o
}

fn check_duplicate_strings(report: &Report) {
    assert_signal_fires(report, "duplicate-strings");
    let ds = report
        .overview
        .duplicate_strings
        .as_ref()
        .expect("duplicate_strings should be populated with find_duplicates=true");
    assert!(
        ds.duplicated_values > 0,
        "expected at least one duplicated string value"
    );
}

// ---------------------------------------------------------------------------
// Scenario 8: Over-capacity HashMap (fires "over-capacity-collections" signal)
// ---------------------------------------------------------------------------

fn build_over_capacity_hashmap() -> Vec<u8> {
    let mut b = HeapBuilder::new();
    heap_builder::build_over_capacity_hashmap(&mut b);
    b.build()
}

fn check_over_capacity_hashmap(report: &Report) {
    assert_signal_fires(report, "over-capacity-collections");
    assert!(
        report.collections.collection_fill_ratio.tracked > 0,
        "expected at least one tracked collection"
    );
}

// ---------------------------------------------------------------------------
// Scenario 9: single_culprit_simple_class (P0-2: holder window fix)
// ---------------------------------------------------------------------------

fn build_single_culprit_simple_class() -> Vec<u8> {
    let mut b = HeapBuilder::new();
    heap_builder::build_single_culprit_simple_class(&mut b);
    b.build()
}

fn check_single_culprit_simple_class(report: &Report) {
    // SimpleValue must appear in biggest_classes (it has 50 instances with payload).
    let cls = report
        .top
        .biggest_classes
        .iter()
        .find(|c| c.pretty_class.contains("SimpleValue"))
        .expect("SimpleValue must appear in biggest_classes");

    // The holder-computation window fix (P0-2): SimpleValue must have non-empty
    // holders even if it's not ranked in the first top_n classes.
    assert!(
        !cls.holders.is_empty(),
        "SimpleValue must have non-empty holders (P0-2: holder window fix)"
    );
}

// ---------------------------------------------------------------------------
// Scenario 10: healthy_baseline (no signals expected)
// ---------------------------------------------------------------------------

fn build_healthy_baseline() -> Vec<u8> {
    let mut b = HeapBuilder::new();
    heap_builder::build_healthy_baseline(&mut b);
    b.build()
}

fn check_healthy_baseline(report: &Report) {
    // The primary value of this scenario: verify that a diffuse heap (no
    // single object > 50% retained) does NOT produce a primary_culprit.
    // Triage signals like "fixed-per-object-overhead" and "gc-root-type"
    // will legitimately fire on a synthetic 5-object heap, so we only assert
    // the report doesn't falsely accuse anyone — not that it's perfectly quiet.
    assert_culprit(report, &CulpritExpectation::NoneExpected);
    // The report-generation round-trip itself (no panic) is the second assertion.
}

// ---------------------------------------------------------------------------
// Scenario 11: threadlocal_pooled_thread_multiple (multiple stale entries)
// ---------------------------------------------------------------------------

fn build_threadlocal_pooled_multiple() -> Vec<u8> {
    let mut b = HeapBuilder::new();
    heap_builder::build_threadlocal_pooled_multiple(&mut b);
    b.build()
}

fn check_threadlocal_pooled_multiple(report: &Report) {
    // The threadlocal-leak signal must fire even when there are multiple stale
    // entries (pool-thread scenario — not just a single stale entry).
    assert_signal_fires(report, "threadlocal-leak");
    assert!(
        report.leak_indicators.thread_local_null_key_count >= 5,
        "expected at least 5 stale ThreadLocal entries; got: {}",
        report.leak_indicators.thread_local_null_key_count,
    );
}

// ---------------------------------------------------------------------------
// Scenario table
// ---------------------------------------------------------------------------

fn default_opts() -> AnalyzeOptions {
    AnalyzeOptions::default()
}

const SCENARIOS: &[Scenario] = &[
    Scenario {
        name: "self_referential_tree",
        opts: default_opts,
        build: build_self_referential,
        check: check_self_referential,
    },
    Scenario {
        name: "empty_oversized_arraylist",
        opts: opts_with_collections,
        build: build_empty_oversized_list,
        check: check_empty_oversized_list,
    },
    Scenario {
        name: "joint_node_thread_headline_bug",
        opts: default_opts,
        build: build_joint_node_thread,
        check: check_joint_node_thread,
    },
    Scenario {
        name: "off_heap_direct_byte_buffer",
        opts: default_opts,
        build: build_off_heap_direct_byte_buffer,
        check: check_off_heap_direct_byte_buffer,
    },
    Scenario {
        name: "threadlocal_null_key_leak",
        opts: default_opts,
        build: build_threadlocal_null_key_leak,
        check: check_threadlocal_null_key_leak,
    },
    Scenario {
        name: "boxed_primitive_bloat",
        opts: default_opts,
        build: build_boxed_primitive_bloat,
        check: check_boxed_primitive_bloat,
    },
    Scenario {
        name: "duplicate_strings",
        opts: opts_with_find_duplicates,
        build: build_duplicate_strings,
        check: check_duplicate_strings,
    },
    Scenario {
        name: "over_capacity_hashmap",
        opts: opts_with_collections,
        build: build_over_capacity_hashmap,
        check: check_over_capacity_hashmap,
    },
    Scenario {
        name: "single_culprit_simple_class",
        opts: default_opts,
        build: build_single_culprit_simple_class,
        check: check_single_culprit_simple_class,
    },
    Scenario {
        name: "healthy_baseline",
        opts: default_opts,
        build: build_healthy_baseline,
        check: check_healthy_baseline,
    },
    Scenario {
        name: "threadlocal_pooled_multiple",
        opts: default_opts,
        build: build_threadlocal_pooled_multiple,
        check: check_threadlocal_pooled_multiple,
    },
];

#[test]
fn scenario_self_referential_tree() {
    run_scenario(&SCENARIOS[0]);
}

#[test]
fn scenario_empty_oversized_arraylist() {
    run_scenario(&SCENARIOS[1]);
}

/// Permanent, named regression test for the joint Node/Thread headline bug
/// (per the plan's P0 item 1 DoD: "a new regression test proving the JOINT
/// bug is resolved"). Kept as its own explicitly-named `#[test]` (not just
/// a table row iterated anonymously) so it shows up by this exact name in
/// `cargo test` output and CI history.
#[test]
fn scenario_joint_node_thread_headline_bug() {
    run_scenario(&SCENARIOS[2]);
}

#[test]
fn scenario_off_heap_direct_byte_buffer() {
    run_scenario(&SCENARIOS[3]);
}

#[test]
fn scenario_threadlocal_null_key_leak() {
    run_scenario(&SCENARIOS[4]);
}

#[test]
fn scenario_boxed_primitive_bloat() {
    run_scenario(&SCENARIOS[5]);
}

#[test]
fn scenario_duplicate_strings() {
    run_scenario(&SCENARIOS[6]);
}

#[test]
fn scenario_over_capacity_hashmap() {
    run_scenario(&SCENARIOS[7]);
}

#[test]
fn scenario_single_culprit_simple_class() {
    run_scenario(&SCENARIOS[8]);
}

#[test]
fn scenario_healthy_baseline() {
    run_scenario(&SCENARIOS[9]);
}

#[test]
fn scenario_threadlocal_pooled_multiple() {
    run_scenario(&SCENARIOS[10]);
}

/// Sanity check that the table and the explicit `#[test]` fns stay in sync
/// (every table row has a dedicated test above) — catches "added a row,
/// forgot the #[test]" at compile-adjacent time rather than silently never
/// running the new scenario.
#[test]
fn all_scenarios_have_a_dedicated_test() {
    assert_eq!(
        SCENARIOS.len(),
        11,
        "update this test when adding scenarios"
    );
}

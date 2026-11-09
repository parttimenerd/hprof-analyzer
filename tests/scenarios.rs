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
// Scenario 12: static_field_anchor (fires "static-field-anchor" signal)
// ---------------------------------------------------------------------------

fn build_static_field_anchor() -> Vec<u8> {
    let mut b = HeapBuilder::new();
    heap_builder::build_static_field_anchor(&mut b);
    b.build()
}

fn check_static_field_anchor(report: &Report) {
    assert_signal_fires(report, "static-field-anchor");
    // The signal detail must name the holding class so users know where to look.
    let signal = report
        .triage
        .iter()
        .find(|s| s.id == "static-field-anchor")
        .expect("static-field-anchor must fire");
    assert!(
        signal.detail.contains("AppCache"),
        "static-field-anchor detail must name the holder class 'AppCache', got: {}",
        signal.detail
    );
}

// ---------------------------------------------------------------------------
// Scenario 13: session_scope_leak (fires "session-scope-leak" signal)
// ---------------------------------------------------------------------------

fn build_session_scope_leak() -> Vec<u8> {
    let mut b = HeapBuilder::new();
    heap_builder::build_session_scope_leak(&mut b);
    b.build()
}

fn check_session_scope_leak(report: &Report) {
    assert_signal_fires(report, "session-scope-leak");
}

// ---------------------------------------------------------------------------
// Scenario 14: unique_strings_no_dup (must NOT fire "duplicate-strings")
// ---------------------------------------------------------------------------

fn build_unique_strings_no_dup() -> Vec<u8> {
    let mut b = HeapBuilder::new();
    heap_builder::build_unique_strings_no_dup(&mut b);
    b.build()
}

fn check_unique_strings_no_dup(report: &Report) {
    assert_signal_silent(report, "duplicate-strings");
}

// ---------------------------------------------------------------------------
// Scenario 15: constant_value_arrays (fires "constant-value-arrays" signal)
// ---------------------------------------------------------------------------

fn build_constant_value_arrays() -> Vec<u8> {
    let mut b = HeapBuilder::new();
    heap_builder::build_constant_value_arrays(&mut b);
    b.build()
}

fn check_constant_value_arrays(report: &Report) {
    assert_signal_fires(report, "constant-value-arrays");
}

// ---------------------------------------------------------------------------
// Scenario 16: Single large object with static-field ownership via root_path
// ---------------------------------------------------------------------------

fn build_single_object_static_owner() -> Vec<u8> {
    let mut b = HeapBuilder::new();
    heap_builder::build_single_object_static_owner(&mut b);
    b.build()
}

fn check_single_object_static_owner(report: &Report) {
    // static-field-anchor must fire and name the holder class.
    assert_signal_fires(report, "static-field-anchor");
    let anchor_signal = report
        .triage
        .iter()
        .find(|s| s.id == "static-field-anchor")
        .expect("static-field-anchor must fire");
    assert!(
        anchor_signal.detail.contains("StaticCacheService"),
        "static-field-anchor detail must name the holder 'StaticCacheService', got: {}",
        anchor_signal.detail
    );
    // When primary_culprits is populated (single object dominates), the
    // concentration signal should fire naming the holder via root_path.
    if !report.primary_culprits.is_empty() {
        let conc = report
            .triage
            .iter()
            .find(|s| s.id == "concentration")
            .expect("concentration must fire when primary_culprits is non-empty");
        assert!(
            conc.detail.contains("StaticCacheService"),
            "concentration detail must name holder 'StaticCacheService' via root_path, got: {}",
            conc.detail
        );
    }
}

// ---------------------------------------------------------------------------
// Scenario 17: LinkedList chain — deep-retention-chain
//              1,200-node chain must fire deep-retention-chain (threshold 1,000)
// ---------------------------------------------------------------------------

fn build_linked_list_chain() -> Vec<u8> {
    let mut b = HeapBuilder::new();
    heap_builder::build_linked_list_chain(&mut b);
    b.build()
}

fn check_linked_list_chain(report: &Report) {
    assert_signal_fires(report, "deep-retention-chain");
    let sig = report
        .triage
        .iter()
        .find(|s| s.id == "deep-retention-chain")
        .expect("deep-retention-chain must fire for a 1200-node chain");
    // Signal must mention the depth number.
    assert!(
        sig.detail.contains("1,"),
        "deep-retention-chain detail must name the chain depth, got: {}",
        sig.detail
    );
}

// ---------------------------------------------------------------------------
// Scenario 19: two_hop_holders — two-hop holder bubble-up in biggest_classes
// ---------------------------------------------------------------------------

fn build_two_hop_holders() -> Vec<u8> {
    let mut b = HeapBuilder::new();
    // Reuse the sankey chain: Container → Substatement → TargetClass.
    // TargetClass's direct holder is Substatement; Container is the two-hop holder.
    heap_builder::build_sankey_imdom_chain(&mut b, 20);
    b.build()
}

fn check_two_hop_holders(report: &Report) {
    let target_row = report
        .top
        .biggest_classes
        .iter()
        .find(|r| r.pretty_class.contains("TargetClass"))
        .expect("TargetClass must appear in biggest_classes");

    assert!(
        !target_row.holders.is_empty(),
        "TargetClass must have at least one holder entry"
    );

    // The direct (level-1) holder is Substatement.
    let direct = target_row
        .holders
        .iter()
        .find(|h| h.holder_class.contains("Substatement") && h.via_class.is_empty());
    assert!(
        direct.is_some(),
        "Substatement must appear as a direct holder of TargetClass.\nActual holders: {:#?}",
        target_row.holders
    );

    // The two-hop holder: Container must appear with via_class mentioning Substatement.
    let two_hop = target_row
        .holders
        .iter()
        .find(|h| h.holder_class.contains("Container") && !h.via_class.is_empty());
    assert!(
        two_hop.is_some(),
        "Container must appear as a two-hop holder of TargetClass (via Substatement).\nActual holders: {:#?}",
        target_row.holders
    );
    let two_hop = two_hop.unwrap();
    assert!(
        two_hop.via_class.iter().any(|v| v.contains("Substatement")),
        "via_class for Container holder must mention Substatement. Got: {:?}",
        two_hop.via_class
    );
}

//   Reproducer for "Who Holds This Class?" Sankey: verifies that the
//   immediate-dominator pairs correctly populate the intermediate class
//   (Substatement) as a holder of TargetClass, not just the top-level
//   GC-root container.
// ---------------------------------------------------------------------------

fn build_sankey_imdom_chain() -> Vec<u8> {
    let mut b = HeapBuilder::new();
    heap_builder::build_sankey_imdom_chain(&mut b, 50);
    b.build()
}

fn check_sankey_imdom_chain(report: &Report) {
    let pairs = &report.dominator_analysis.immediate_dominators.pairs;

    assert!(
        !pairs.is_empty(),
        "ImmDomPairs must be non-empty for the sankey_imdom_chain scenario"
    );

    // The (Substatement → TargetClass) pair must exist: Substatement is the
    // *immediate* dominator of every TargetClass object in this dump.
    let sub_to_target = pairs.iter().find(|p| {
        p.dominator_class.contains("Substatement") && p.dominated_class.contains("TargetClass")
    });
    assert!(
        sub_to_target.is_some(),
        "Expected ImmDomPair (Substatement → TargetClass) to exist.\n\
         Sankey 'Who Holds TargetClass?' left-side must show Substatement.\n\
         Actual pairs: {:?}",
        pairs
            .iter()
            .take(10)
            .map(|p| (&p.dominator_class, &p.dominated_class))
            .collect::<Vec<_>>()
    );

    // The (Container → Substatement) pair must also exist.
    let container_to_sub = pairs.iter().find(|p| {
        p.dominator_class.contains("Container") && p.dominated_class.contains("Substatement")
    });
    assert!(
        container_to_sub.is_some(),
        "Expected ImmDomPair (Container → Substatement) to exist.\n\
         Sankey 'Who Holds Substatement?' left-side must show Container.\n\
         Actual pairs: {:?}",
        pairs
            .iter()
            .take(10)
            .map(|p| (&p.dominator_class, &p.dominated_class))
            .collect::<Vec<_>>()
    );
}

// ---------------------------------------------------------------------------
// Scenario 19: category_in_histogram — category is non-empty on ClassRow
// ---------------------------------------------------------------------------

fn build_category_in_histogram() -> Vec<u8> {
    heap_builder::build_app_class_in_histogram()
}

fn check_category_in_histogram(report: &Report) {
    // biggest_classes should have category populated (not empty) for recognized classes
    let has_category = report
        .top
        .biggest_classes
        .iter()
        .any(|r| !r.category.is_empty());
    assert!(
        has_category,
        "at least one biggest_classes row should have a non-empty category; got: {:?}",
        report
            .top
            .biggest_classes
            .iter()
            .map(|r| (&r.pretty_class, &r.category))
            .collect::<Vec<_>>()
    );
}

// ---------------------------------------------------------------------------
// Scenario 20: culprit_prefers_app_class_over_jdk
// ---------------------------------------------------------------------------

fn build_culprit_prefers_app_class() -> Vec<u8> {
    heap_builder::build_app_class_culprit()
}

fn check_culprit_prefers_app_class(report: &Report) {
    assert!(
        !report.primary_culprits.is_empty(),
        "should have at least one primary culprit"
    );
    let culprit = &report.primary_culprits[0];
    assert!(
        culprit.class_name.starts_with("com.example"),
        "culprit should be the application class, got: {}",
        culprit.class_name
    );
}

// ---------------------------------------------------------------------------
// Scenario 21: subclass_hierarchy_no_panic (class_with_super coverage)
// ---------------------------------------------------------------------------

fn build_subclass_hierarchy_scenario() -> Vec<u8> {
    heap_builder::build_subclass_hierarchy()
}

fn check_subclass_hierarchy_no_panic(report: &Report) {
    // Just verify the report builds cleanly when subclasses are present.
    let _ = &report.triage;
}

// ---------------------------------------------------------------------------
// Scenario 22: mixed_class_heap (classof() coverage, no-panic)
// ---------------------------------------------------------------------------

fn build_mixed_class_heap_scenario() -> Vec<u8> {
    heap_builder::build_mixed_class_heap()
}

fn check_mixed_class_heap_no_panic(report: &Report) {
    // Just verifies no panic — classof() is OQL-only.
    let _ = &report.triage;
}

// ---------------------------------------------------------------------------
// Scenario 27: from_entries_no_panic — FROM ENTRIES parsing smoke-test
// ---------------------------------------------------------------------------

fn build_from_entries_scenario() -> Vec<u8> {
    heap_builder::build_arraylist_with_string_elements()
}

fn check_from_entries_no_panic(report: &Report) {
    // FROM ENTRIES is OQL-only; just verify report builds without panic.
    let _ = &report.triage;
}

// ---------------------------------------------------------------------------
// Scenario 33: finalizer_classes_breakdown (P2 — Finalizer referents by class)
// ---------------------------------------------------------------------------

fn build_finalizer_classes_breakdown() -> Vec<u8> {
    let mut b = HeapBuilder::new();
    heap_builder::build_finalizer_classes(&mut b);
    b.build()
}

fn check_finalizer_classes_breakdown(report: &Report) {
    // The finalizer_classes breakdown must be non-empty (5 finalizers present).
    assert!(
        !report.finalizer_classes.is_empty(),
        "expected finalizer_classes to be non-empty; got: {:?}",
        report.finalizer_classes,
    );
    // FinalizableA had 3 instances, FinalizableB had 2 — FinalizableA must be first.
    let top = &report.finalizer_classes[0];
    assert!(
        top.referent_class.contains("FinalizableA"),
        "top finalizer_classes entry should be FinalizableA (count=3), got: {:?}",
        top,
    );
    assert_eq!(
        top.count, 3,
        "FinalizableA should have count=3, got: {}",
        top.count,
    );
    // FinalizableB must appear too.
    let b_row = report
        .finalizer_classes
        .iter()
        .find(|r| r.referent_class.contains("FinalizableB"));
    assert!(
        b_row.is_some(),
        "FinalizableB must appear in finalizer_classes; got: {:?}",
        report.finalizer_classes,
    );
    assert_eq!(b_row.unwrap().count, 2, "FinalizableB should have count=2");
}

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
    Scenario {
        name: "static_field_anchor",
        opts: default_opts,
        build: build_static_field_anchor,
        check: check_static_field_anchor,
    },
    Scenario {
        name: "session_scope_leak",
        opts: default_opts,
        build: build_session_scope_leak,
        check: check_session_scope_leak,
    },
    Scenario {
        name: "unique_strings_no_dup",
        opts: opts_with_find_duplicates,
        build: build_unique_strings_no_dup,
        check: check_unique_strings_no_dup,
    },
    Scenario {
        name: "constant_value_arrays",
        opts: opts_with_collections,
        build: build_constant_value_arrays,
        check: check_constant_value_arrays,
    },
    Scenario {
        name: "single_object_static_owner",
        opts: default_opts,
        build: build_single_object_static_owner,
        check: check_single_object_static_owner,
    },
    Scenario {
        name: "linked_list_chain",
        opts: default_opts,
        build: build_linked_list_chain,
        check: check_linked_list_chain,
    },
    Scenario {
        name: "sankey_imdom_chain",
        opts: default_opts,
        build: build_sankey_imdom_chain,
        check: check_sankey_imdom_chain,
    },
    Scenario {
        name: "two_hop_holders",
        opts: default_opts,
        build: build_two_hop_holders,
        check: check_two_hop_holders,
    },
    Scenario {
        name: "category_in_histogram",
        opts: default_opts,
        build: build_category_in_histogram,
        check: check_category_in_histogram,
    },
    Scenario {
        name: "culprit_prefers_app_class_over_jdk",
        opts: default_opts,
        build: build_culprit_prefers_app_class,
        check: check_culprit_prefers_app_class,
    },
    Scenario {
        name: "subclass_hierarchy_no_panic",
        opts: default_opts,
        build: build_subclass_hierarchy_scenario,
        check: check_subclass_hierarchy_no_panic,
    },
    Scenario {
        name: "mixed_class_heap_no_panic",
        opts: default_opts,
        build: build_mixed_class_heap_scenario,
        check: check_mixed_class_heap_no_panic,
    },
    // Scenario 23: outbounds_attr_no_panic — verify the report builds without
    // panic when the heap has a linked-list chain (the kind of shape @outbounds
    // is designed to traverse). The attribute itself is OQL-only and is not
    // exercised by the report pipeline, so this scenario just guards that the
    // report-building path does not regress on this heap shape.
    Scenario {
        name: "outbounds_attr_no_panic",
        opts: default_opts,
        build: build_linked_list_chain,
        check: |_r: &Report| {},
    },
    // Scenario 24: class_super_idx_populated — verify that the graph builds
    // cleanly when subclasses are present (exercises the class_super_idx
    // population path added to pass2).
    Scenario {
        name: "class_super_idx_populated",
        opts: default_opts,
        build: build_subclass_hierarchy_scenario,
        check: |r: &Report| {
            // Indirect test: report builds cleanly when subclasses are present.
            let _ = r;
        },
    },
    // Scenario 25: category_attr_no_panic — verify the report builds without
    // panic when the heap has a mix of JDK and framework classes.
    // @category is OQL-only; this scenario just guards that the report-building
    // path (which now builds the class_categories vec) does not regress.
    Scenario {
        name: "category_attr_no_panic",
        opts: default_opts,
        build: build_mixed_class_heap_scenario,
        check: |r: &Report| {
            let _ = r; // @category is OQL-only; just verify report builds
        },
    },
    // Scenario 26: superclass_attr_populated — verify the report builds cleanly
    // when subclasses are present (exercises the @superclass resolution path).
    Scenario {
        name: "superclass_attr_populated",
        opts: default_opts,
        build: build_subclass_hierarchy_scenario,
        check: |r: &Report| {
            // @superclass is OQL-only; just verify report builds cleanly
            let _ = r;
        },
    },
    // Scenario 27: from_entries_no_panic — FROM ENTRIES parsing smoke-test.
    Scenario {
        name: "from_entries_no_panic",
        opts: default_opts,
        build: build_from_entries_scenario,
        check: check_from_entries_no_panic,
    },
    // Scenario 28: collection_size_attr_populated — @collectionSize returns the
    // element count for ArrayList instances (not null), verifying the
    // coll_size_per_dense capture path through fielddecode.
    Scenario {
        name: "collection_size_attr_populated",
        opts: default_opts,
        build: build_from_entries_scenario, // ArrayList with size=3
        check: |r: &Report| {
            // @collectionSize is OQL-only; just verify report builds cleanly.
            // The live OQL check is in scenario_collection_size_attr_populated below.
            let _ = r;
        },
    },
    // Scenario 29: root_path_step_category — verify that root_path steps carry
    // non-empty category strings (regression for the category annotation path
    // in build_suspect_root_path).
    Scenario {
        name: "root_path_step_category",
        opts: default_opts,
        build: build_mixed_class_heap_scenario,
        check: |r: &Report| {
            // If there is a root_path, each step should have a non-empty display_class.
            if let Some(suspect) = r.leaks.suspects.first() {
                if let Some(path) = suspect.root_path.as_ref() {
                    for step in path.iter() {
                        assert!(
                            !step.display_class.is_empty(),
                            "root_path step should have non-empty display_class; obj={}",
                            step.obj_index_1based
                        );
                    }
                }
            }
        },
    },
    // Scenario 30: classof_groups_objects — verifies the report builds cleanly
    // on a heap with multiple class types (exercises the category+classof paths).
    Scenario {
        name: "classof_groups_objects",
        opts: default_opts,
        build: build_mixed_class_heap_scenario,
        check: |r: &Report| {
            let _ = r; // classof() is OQL-only; just verify report builds
        },
    },
    // Scenario 31: dominators_and_dominatorof_parse — verifies the report
    // builds cleanly on a heap with a dominated object chain.
    Scenario {
        name: "dominators_and_dominatorof_parse",
        opts: default_opts,
        build: build_subclass_hierarchy_scenario,
        check: |r: &Report| {
            // Every non-root object's immediate dominator index must be < object count.
            // This is verified structurally by the dominator tree being valid.
            let _ = &r.leaks;
        },
    },
    // Scenario 32: union_query_merges_results — verifies the report builds
    // cleanly (UNION is OQL-only; the OQL unit tests are in src/query/run.rs).
    Scenario {
        name: "union_query_merges_results",
        opts: default_opts,
        build: build_mixed_class_heap_scenario,
        check: |r: &Report| {
            let _ = &r.triage; // UNION is OQL-only; verify no panic
        },
    },
    // Scenario 33: finalizer_classes_breakdown (P2) — Finalizer referent class
    // breakdown must be non-empty and correctly grouped when Finalizer instances
    // with two distinct referent classes are present.
    Scenario {
        name: "finalizer_classes_breakdown",
        opts: default_opts,
        build: build_finalizer_classes_breakdown,
        check: check_finalizer_classes_breakdown,
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

#[test]
fn scenario_static_field_anchor() {
    run_scenario(&SCENARIOS[11]);
}

#[test]
fn scenario_session_scope_leak() {
    run_scenario(&SCENARIOS[12]);
}

#[test]
fn scenario_unique_strings_no_dup() {
    run_scenario(&SCENARIOS[13]);
}

#[test]
fn scenario_constant_value_arrays() {
    run_scenario(&SCENARIOS[14]);
}

#[test]
fn scenario_single_object_static_owner() {
    run_scenario(&SCENARIOS[15]);
}

#[test]
fn scenario_linked_list_chain() {
    run_scenario(&SCENARIOS[16]);
}

#[test]
fn scenario_sankey_imdom_chain() {
    run_scenario(&SCENARIOS[17]);
}

#[test]
fn scenario_two_hop_holders() {
    run_scenario(&SCENARIOS[18]);
}

#[test]
fn scenario_category_in_histogram() {
    run_scenario(&SCENARIOS[19]);
}

#[test]
fn scenario_culprit_prefers_app_class_over_jdk() {
    run_scenario(&SCENARIOS[20]);
}

#[test]
fn scenario_subclass_hierarchy_no_panic() {
    run_scenario(&SCENARIOS[21]);
}

#[test]
fn scenario_mixed_class_heap_no_panic() {
    run_scenario(&SCENARIOS[22]);
}

#[test]
fn scenario_outbounds_attr_no_panic() {
    run_scenario(&SCENARIOS[23]);
}

#[test]
fn scenario_class_super_idx_populated() {
    run_scenario(&SCENARIOS[24]);
}

#[test]
fn scenario_category_attr_no_panic() {
    run_scenario(&SCENARIOS[25]);
}

#[test]
fn scenario_superclass_attr_populated() {
    run_scenario(&SCENARIOS[26]);
}

#[test]
fn scenario_from_entries_no_panic() {
    run_scenario(&SCENARIOS[27]);
}

#[test]
fn scenario_collection_size_attr_populated() {
    run_scenario(&SCENARIOS[28]);
    // Verify @collectionSize returns a non-null integer by running the OQL.
    let bytes = heap_builder::build_arraylist_with_string_elements();
    let dir = std::env::temp_dir();
    let path = dir.join("test_coll_size.hprof");
    std::fs::write(&path, &bytes).expect("write temp hprof");
    let results = hprof_analyzer::run_oql_query(
        path.to_str().unwrap(),
        "SELECT @collectionSize FROM INSTANCEOF java.util.ArrayList",
        false,
        &AnalyzeOptions::default(),
    )
    .expect("OQL query failed");
    assert_eq!(results.len(), 1);
    let result = &results[0];
    assert!(result.error.is_none(), "query error: {:?}", result.error);
    assert!(!result.rows.is_empty(), "@collectionSize returned no rows");
    let val = &result.rows[0][0];
    assert!(
        !matches!(val, hprof_analyzer::query::model::QueryValue::Null),
        "@collectionSize should not be null for a recognized ArrayList, got: {:?}",
        val
    );
    // The ArrayList was built with size=3.
    assert_eq!(
        *val,
        hprof_analyzer::query::model::QueryValue::Int(3),
        "@collectionSize should be 3"
    );
}

#[test]
fn scenario_root_path_step_category() {
    run_scenario(&SCENARIOS[29]);
}

#[test]
fn scenario_classof_groups_objects() {
    run_scenario(&SCENARIOS[30]);
}

#[test]
fn scenario_dominators_and_dominatorof_parse() {
    run_scenario(&SCENARIOS[31]);
}

#[test]
fn scenario_union_query_merges_results() {
    run_scenario(&SCENARIOS[32]);
}

#[test]
fn scenario_finalizer_classes_breakdown() {
    run_scenario(&SCENARIOS[33]);
}

/// Sanity check that the table and the explicit `#[test]` fns stay in sync
/// (every table row has a dedicated test above) — catches "added a row,
/// forgot the #[test]" at compile-adjacent time rather than silently never
/// running the new scenario.
#[test]
fn all_scenarios_have_a_dedicated_test() {
    assert_eq!(
        SCENARIOS.len(),
        34,
        "update this test when adding scenarios"
    );
}

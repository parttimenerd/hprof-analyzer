//! Structural size-budget guard for the canonical JSON report (Phase B).
//!
//! INVARIANT PROTECTED BY THIS TEST
//! --------------------------------
//! The canonical JSON report must NEVER contain an unbounded *per-object*
//! array. A heap dump can hold hundreds of millions of objects, so any `Vec`
//! that grows with the object count would make the JSON explode. Every list in
//! the report is instead bounded by one of:
//!   * the number of loaded CLASSES (the histogram: one row per class), or
//!   * a fixed top-N / threshold cap (top consumers, biggest objects, leak
//!     suspects), or
//!   * the longest dominator chain (the depth histogram).
//!
//! This test is the structural guard: it asserts the emitted JSON stays bounded
//! relative to the class count and the fixed top-N caps, and crucially that its
//! size tracks CLASSES, not OBJECTS. If someone accidentally adds a per-object
//! array the serialized size would blow past these budgets and this test fails.
//!
//! EMPIRICAL BASIS (measured 2026-07-13, `dump_8_log-regression.hprof`)
//! -------------------------------------------------------------------
//!   total_objects              = 850_496
//!   histogram rows (~#classes) =  22_412   (classes_loaded = 19_871)
//!   dominator_depth_histogram  =     967
//!   leaks.suspects             =       1
//!   top.biggest_objects        =      20
//!   top.biggest_classes        =      20
//!   total JSON bytes           = 3_861_259  (~172 bytes / histogram row)
//!
//! Cross-check (`dump_0_fj-kmeans.hprof`): 3.2M objects but only ~2.6K classes
//! produces a ~0.4 MB JSON — i.e. ~4x MORE objects than dump_8 yet ~9x SMALLER
//! JSON. That is the whole point: JSON size follows CLASSES, not OBJECTS.
//!
//! Thresholds below are chosen with 5-10x headroom over the measured values so
//! the test is not flaky, but far enough below `total_objects` that a genuine
//! per-object array (which would add ~tens of bytes per object) trips them.

use std::path::Path;
use std::process::Command;

use serde_json::Value;

/// Largest available fixture -> strongest signal (most classes + objects).
const FIXTURE: &str = "dump_8_log-regression.hprof";

/// A per-object array of even the smallest rows would add many bytes per
/// object. This bounds serialized bytes-per-histogram-row well above the
/// measured ~172 while staying far below the per-object cost we want to catch.
const MAX_BYTES_PER_HISTOGRAM_ROW: usize = 2_000;

/// Fixed additive slack (bytes) for the non-histogram, non-list parts of the
/// report (overview scalars, package tree, top-N lists, formatting).
const FIXED_JSON_SLACK: usize = 2_000_000;

/// Generous cap for every top-N / threshold-bounded list. Measured lengths are
/// 1..=20; a real per-object list would be in the millions.
const MAX_TOP_N_LEN: usize = 1_000;

/// Cap for the dominator-depth histogram (bounded by the longest dominator
/// chain). Measured 967; a pathological-but-legal chain stays well under this.
const MAX_DOMINATOR_DEPTH_LEN: usize = 10_000;

/// The histogram is at most a small multiple of the loaded-class count (array
/// element types etc. add distinct rows). Objects must outnumber histogram rows
/// by a wide margin on any non-trivial dump; require at least this ratio so the
/// test proves the histogram does NOT scale with objects.
const MIN_OBJECTS_PER_HISTOGRAM_ROW: u64 = 20;

fn arr_len(v: &Value, path: &[&str]) -> usize {
    let mut cur = v;
    for key in path {
        cur = &cur[key];
    }
    cur.as_array().map(|a| a.len()).unwrap_or(0)
}

#[test]
fn json_size_budget() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let hprof = dir.join(FIXTURE);

    // Mirror the parity/integration skip guard: if a dev checked out without
    // the (large) LFS fixtures, or they are unsmudged pointers, skip cleanly.
    if !hprof.exists()
        || std::fs::metadata(&hprof)
            .map(|m| m.len() < 1024)
            .unwrap_or(true)
    {
        eprintln!(
            "skipping json_size_budget: fixture missing or unsmudged LFS pointer at {}",
            hprof.display()
        );
        return;
    }

    let out = Command::new(env!("CARGO_BIN_EXE_hprof-analyzer"))
        .arg("analyze")
        .arg(&hprof)
        .arg("--format")
        .arg("json")
        .arg("--compress")
        .arg("none")
        .output()
        .unwrap_or_else(|e| panic!("failed to run analyzer on {FIXTURE}: {e}"));
    assert!(
        out.status.success(),
        "{FIXTURE} exited non-zero: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let json_bytes = out.stdout.len();
    let json: Value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("analyzer stdout was not valid JSON: {e}"));

    // --- The analyzer's own reported object and class counts. ---
    let total_objects = json["overview"]["total_objects"]
        .as_u64()
        .expect("overview.total_objects missing or not a number");
    let histogram_len = arr_len(&json, &["overview", "histogram"]);
    assert!(
        histogram_len > 0,
        "overview.histogram is empty; cannot validate the size budget"
    );
    let class_count = histogram_len as u64;

    // Sanity: this guard is only meaningful on a dump where objects vastly
    // outnumber classes (otherwise "bounded by classes" is trivially small).
    assert!(
        total_objects > class_count * MIN_OBJECTS_PER_HISTOGRAM_ROW,
        "fixture too small to guard: total_objects={total_objects} is not far larger \
         than histogram rows={class_count} (need > {MIN_OBJECTS_PER_HISTOGRAM_ROW}x). \
         Pick a larger fixture."
    );

    // --- (a) The histogram is a per-CLASS array, NOT per-object. ---
    // On a large dump objects dwarf classes; require a wide margin so a rogue
    // per-object array masquerading as the histogram cannot slip through.
    assert!(
        (histogram_len as u64) * MIN_OBJECTS_PER_HISTOGRAM_ROW < total_objects,
        "overview.histogram has {histogram_len} rows, which is NOT far below \
         total_objects={total_objects}. A per-object array may have leaked into \
         the histogram (it must be bounded by the class count)."
    );

    // --- (b) Every top-N / threshold-bounded list stays under a fixed cap. ---
    for path in [
        vec!["leaks", "suspects"],
        vec!["top", "biggest_objects"],
        vec!["top", "biggest_classes"],
    ] {
        let len = arr_len(&json, &path);
        assert!(
            len <= MAX_TOP_N_LEN,
            "top-N list {path:?} has {len} entries, exceeding the fixed cap of \
             {MAX_TOP_N_LEN}. Top-N lists must never scale with object count \
             (total_objects={total_objects})."
        );
    }

    // --- (c) The dominator-depth histogram is bounded by the longest chain. ---
    let ddh_len = arr_len(&json, &["overview", "dominator_depth_histogram"]);
    assert!(
        ddh_len <= MAX_DOMINATOR_DEPTH_LEN && (ddh_len as u64) < total_objects,
        "overview.dominator_depth_histogram has {ddh_len} rows (cap \
         {MAX_DOMINATOR_DEPTH_LEN}); it must be bounded by the longest dominator \
         chain, never by object count (total_objects={total_objects})."
    );

    // --- KEY assertion: total JSON size tracks CLASSES, not OBJECTS. ---
    // Budget = per-histogram-row bytes * #classes + fixed slack for scalars,
    // the package tree, and the top-N lists. A per-object array would add tens
    // of bytes * millions of objects and blow far past this.
    let budget = histogram_len
        .saturating_mul(MAX_BYTES_PER_HISTOGRAM_ROW)
        .saturating_add(FIXED_JSON_SLACK);
    assert!(
        json_bytes <= budget,
        "JSON is {json_bytes} bytes, exceeding the class-bounded budget of \
         {budget} bytes ({MAX_BYTES_PER_HISTOGRAM_ROW} B/row * {histogram_len} \
         rows + {FIXED_JSON_SLACK} slack). The report may contain an unbounded \
         per-object array (total_objects={total_objects})."
    );
}

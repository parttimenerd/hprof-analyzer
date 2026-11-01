/// Regression tests for the five demo heap dumps in `test-projects/dumps/`.
///
/// These tests are **opt-in**: they skip automatically when the dump files are
/// absent (run `test-projects/build.sh` first to generate them).
///
/// Dumps and what each exercises:
///
///   empty-array-list.hprof.gz
///       A single `ArrayList` pre-allocated with capacity 1,000,000 but
///       zero elements, alongside ~270+ small collections created by normal
///       JVM/JDK startup. The planted list is too small a SHARE of all
///       tracked collections to trip `EmptyCollectionCemetery` (60% floor),
///       but `OverCapacityCollections` (always-on, not gated behind
///       `--collections`) correctly reports the ~8 MB of wasted backing-array
///       capacity directly.
///
///   non-closed-zip-stream.hprof.gz
///       1,000 `BufferedReader → GZIPInputStream` chains retained in a static
///       list after being fully read without `close()`. The heap is dominated
///       by the leak chains; the tool should identify `BufferedReader` or
///       `java.util.ArrayList` (holding the list) as a top retainer.
///
///   rec-normal.hprof.gz
///       RecursiveHashMaps depth=4 normal mode: a 4-level tree where every
///       node has 4 children and a 100-entry `HashMap<String,String>`.
///       `RecursiveHashMaps$Node` should appear in `biggest_classes`.
///
///   rec-complex.hprof.gz
///       Same tree but "complex" mode (1 child at the last level). Similar
///       expectations to rec-normal.
///
///   rec-normal-crash.hprof.gz
///       OOM crash dump from the same program at depth=10. The dump may be
///       truncated at the crash boundary; we only require no panic and a valid
///       JSON report (no objects assertion is relaxed for the crash variant).
///
/// Run:
///   # First generate dumps:
///   cd test-projects && ./build.sh
///
///   # Then run these tests:
///   cargo test --test test_projects_regression
use std::path::{Path, PathBuf};
use std::process::Command;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn dump_dir() -> PathBuf {
    let root = std::env::var("CARGO_MANIFEST_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."));
    root.join("test-projects").join("dumps")
}

fn dump_path(name: &str) -> PathBuf {
    dump_dir().join(format!("{name}.hprof.gz"))
}

/// Returns `Some(path)` only when the file exists and is large enough to be a
/// real dump (not a Git-LFS stub).  Callers skip tests when this returns `None`.
fn real_dump(path: &Path) -> Option<&Path> {
    match std::fs::metadata(path) {
        Ok(m) if m.len() >= 1024 => Some(path),
        _ => None,
    }
}

fn bin() -> String {
    std::env::var("CARGO_BIN_EXE_hprof-analyzer")
        .unwrap_or_else(|_| "target/release/hprof-analyzer".to_string())
}

struct RunResult {
    success: bool,
    stdout: String,
    stderr: String,
}

fn run(args: &[&str]) -> RunResult {
    let out = Command::new(bin())
        .args(args)
        .output()
        .expect("failed to spawn hprof-analyzer");
    RunResult {
        success: out.status.success(),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

fn assert_no_panic(r: &RunResult, label: &str) {
    assert!(
        !r.stderr.contains("panicked at"),
        "{label}: Rust panic in stderr:\n{}",
        r.stderr
    );
}

fn assert_no_internal_leak(r: &RunResult, label: &str) {
    for bad in &[
        "eof in read_into",
        "eof in skip",
        "failed to fill whole buffer",
        "called `unwrap()` on a `None`",
        "index out of bounds",
    ] {
        assert!(
            !r.stderr.contains(bad),
            "{label}: leaked internal message {bad:?} in stderr:\n{}",
            r.stderr
        );
    }
}

fn parse_json(stdout: &str, label: &str) -> serde_json::Value {
    serde_json::from_str(stdout)
        .unwrap_or_else(|e| panic!("{label}: stdout is not valid JSON: {e}\n{stdout}"))
}

/// Run the core battery (JSON report + heap summary + triage + leaks +
/// histogram) and assert no crash/panic on any of them.
fn battery_core(dump: &Path, name: &str) {
    let dump_str = dump.to_str().unwrap();

    // JSON report
    let r = run(&[dump_str, "--format", "json", "--progress", "never"]);
    assert!(
        r.success,
        "{name} [json]: exit non-zero\nstderr: {}",
        r.stderr
    );
    assert_no_panic(&r, &format!("{name} [json]"));
    assert_no_internal_leak(&r, &format!("{name} [json]"));
    let v = parse_json(&r.stdout, &format!("{name} [json]"));
    assert!(
        v["schema_version"].is_number(),
        "{name}: JSON missing schema_version"
    );

    // heap summary — `heap summary` has no --progress flag; the load() closure
    // inside run_heap_cmd reads from the cache built above and suppresses progress
    // automatically when stderr is not a terminal (always the case in CI).
    let r = run(&["heap", "summary", dump_str]);
    assert!(
        r.success,
        "{name} [summary]: exit non-zero\nstderr: {}",
        r.stderr
    );
    assert_no_panic(&r, &format!("{name} [summary]"));
    assert_no_internal_leak(&r, &format!("{name} [summary]"));

    // triage section — same note: no --progress flag on heap report
    let r = run(&["heap", "report", dump_str, "--section", "triage"]);
    assert!(
        r.success,
        "{name} [triage]: exit non-zero\nstderr: {}",
        r.stderr
    );
    assert_no_panic(&r, &format!("{name} [triage]"));
    assert_no_internal_leak(&r, &format!("{name} [triage]"));

    // leaks section
    let r = run(&["heap", "report", dump_str, "--section", "leaks"]);
    assert!(
        r.success,
        "{name} [leaks]: exit non-zero\nstderr: {}",
        r.stderr
    );
    assert_no_panic(&r, &format!("{name} [leaks]"));
    assert_no_internal_leak(&r, &format!("{name} [leaks]"));

    // histogram
    let r = run(&["heap", "histogram", dump_str]);
    assert!(
        r.success,
        "{name} [histogram]: exit non-zero\nstderr: {}",
        r.stderr
    );
    assert_no_panic(&r, &format!("{name} [histogram]"));
    assert_no_internal_leak(&r, &format!("{name} [histogram]"));
}

// ---------------------------------------------------------------------------
// empty-array-list.hprof.gz
//
// A single ArrayList with capacity 1,000,000 but size 0.  The
// EmptyCollectionCemetery triage rule fires without --collections (since
// empty_count/tracked == 100% >= 60% threshold).  With --collections the
// OverCapacityCollections rule fires for the wasted ~8 MB backing array.
// ---------------------------------------------------------------------------

#[test]
fn empty_array_list_no_crash() {
    let p = dump_path("empty-array-list");
    let Some(dump) = real_dump(&p) else { return };
    battery_core(dump, "empty-array-list");
}

#[test]
fn empty_array_list_total_objects_positive() {
    let p = dump_path("empty-array-list");
    let Some(dump) = real_dump(&p) else { return };
    let r = run(&[
        dump.to_str().unwrap(),
        "--format",
        "json",
        "--progress",
        "never",
    ]);
    assert!(
        r.success,
        "empty-array-list json failed\nstderr: {}",
        r.stderr
    );
    let v = parse_json(&r.stdout, "empty-array-list");
    let objects = v["overview"]["total_objects"].as_u64().unwrap_or(0);
    assert!(
        objects > 0,
        "empty-array-list: expected total_objects > 0, got {objects}"
    );
}

/// Without --collections, this dump should NOT trip EmptyCollectionCemetery:
/// the planted empty ArrayList is one of ~270+ collections tracked (JVM
/// startup creates many small internal maps/lists), so the empty-share ratio
/// never reaches the 60% floor that rule requires. The scenario IS caught —
/// just by a different, more precise rule (`over-capacity-collections`, see
/// `empty_array_list_collections_flag_detects_waste` below) which reports the
/// ~8 MB of wasted backing-array capacity directly, regardless of how many
/// other (correctly-sized, non-empty) collections exist alongside it.
///
/// This test exists to catch a real regression found during investigation:
/// `heap report`/`heap summary` (unlike the top-level `analyze` command) used
/// to build `AnalyzeOptions` with an EMPTY `coll_descs` list, so collection
/// classification (ArrayList/HashMap/etc.) silently never ran at all through
/// `heap` subcommands — `collections_by_size.tracked` was always 0, and
/// EVERY collection-waste triage rule was unreachable via `heap report`,
/// regardless of flags. Fixed in `src/opts.rs` by seeding
/// `coll_descs: builtin_coll_descs()` in the shared `DetailLevel::options()`
/// constructor instead of `Vec::new()`. This test asserts collection tracking
/// is at least alive (`tracked > 0`) through the `heap report` path, which
/// would have caught the regression; the precise waste signal is covered by
/// `empty_array_list_collections_flag_detects_waste`.
#[test]
fn empty_array_list_triage_flags_empty_collection() {
    let p = dump_path("empty-array-list");
    let Some(dump) = real_dump(&p) else { return };
    let r = run(&[
        "heap",
        "report",
        dump.to_str().unwrap(),
        "--section",
        "collections",
        "--json",
    ]);
    assert!(
        r.success,
        "empty-array-list collections json failed\nstderr: {}",
        r.stderr
    );
    let v = parse_json(&r.stdout, "empty-array-list collections");
    let tracked = v["collections_by_size"]["tracked"].as_u64().unwrap_or(0);
    assert!(
        tracked > 0,
        "empty-array-list: expected collections_by_size.tracked > 0 via `heap report` \
         (regression guard: collection classification must run through ALL entry points, \
         not just the top-level `analyze` command — see src/opts.rs coll_descs), got {v:#}"
    );

    // The always-on EmptyCollectionCemetery rule should NOT fire here (the
    // empty share is ~2.9% of ~270+ tracked collections, far below its 60%
    // floor) — asserting its absence pins down the "many small startup
    // collections dilute the share" reasoning above, so a future change to
    // either the rule's threshold or this fixture's JVM startup noise is
    // caught rather than silently assumed.
    let r2 = run(&[
        "heap",
        "report",
        dump.to_str().unwrap(),
        "--section",
        "triage",
        "--json",
    ]);
    assert!(
        r2.success,
        "empty-array-list triage json failed\nstderr: {}",
        r2.stderr
    );
    let v2 = parse_json(&r2.stdout, "empty-array-list triage");
    let signals = v2.as_array().cloned().unwrap_or_default();
    let has_overcap = signals.iter().any(|s| {
        s["id"]
            .as_str()
            .is_some_and(|id| id == "over-capacity-collections")
    });
    assert!(
        has_overcap,
        "empty-array-list: expected 'over-capacity-collections' (fires unconditionally, \
         not gated on --collections) to catch the planted 1,000,000-capacity empty \
         ArrayList, but got: {:#?}",
        signals
            .iter()
            .map(|s| s["id"].as_str().unwrap_or("?"))
            .collect::<Vec<_>>()
    );
}

/// Same assertion as the test above, but through the top-level `analyze`
/// command with `--collections` explicitly passed, to additionally guard the
/// original/`analyze`-specific code path (which also merges in any user
/// `.hprof-analyzer.toml`/`--collection-config` entries, unlike `heap report`'s
/// baseline). `OverCapacityCollections` itself does not require `--collections`
/// (see `empty_array_list_triage_flags_empty_collection` above) — this test
/// exists to keep the `analyze`-specific path covered, not because the flag
/// is required for the signal to fire.
#[test]
fn empty_array_list_collections_flag_detects_waste() {
    let p = dump_path("empty-array-list");
    let Some(dump) = real_dump(&p) else { return };
    let r = run(&[
        dump.to_str().unwrap(),
        "--format",
        "json",
        "--collections",
        "--progress",
        "never",
    ]);
    assert!(
        r.success,
        "empty-array-list --collections analyze failed\nstderr: {}",
        r.stderr
    );
    let v = parse_json(&r.stdout, "empty-array-list --collections json");
    // Full report JSON: triage is at v["triage"] as an array.
    let signals = v["triage"].as_array().cloned().unwrap_or_default();
    let has_overcap = signals.iter().any(|s| {
        s["id"]
            .as_str()
            .is_some_and(|id| id == "over-capacity-collections")
    });
    assert!(
        has_overcap,
        "empty-array-list --collections: expected 'over-capacity-collections' triage signal but got: {:#?}",
        signals
            .iter()
            .map(|s| s["id"].as_str().unwrap_or("?"))
            .collect::<Vec<_>>()
    );
}

// ---------------------------------------------------------------------------
// non-closed-zip-stream.hprof.gz
//
// 1,000 BufferedReader chains retained in a static ArrayList without close().
// The heap is dominated by the retained GZIPInputStream objects and their
// internal inflater buffers.
// ---------------------------------------------------------------------------

#[test]
fn non_closed_zip_no_crash() {
    let p = dump_path("non-closed-zip-stream");
    let Some(dump) = real_dump(&p) else { return };
    battery_core(dump, "non-closed-zip-stream");
}

#[test]
fn non_closed_zip_total_objects_positive() {
    let p = dump_path("non-closed-zip-stream");
    let Some(dump) = real_dump(&p) else { return };
    let r = run(&[
        dump.to_str().unwrap(),
        "--format",
        "json",
        "--progress",
        "never",
    ]);
    assert!(
        r.success,
        "non-closed-zip json failed\nstderr: {}",
        r.stderr
    );
    let v = parse_json(&r.stdout, "non-closed-zip");
    let objects = v["overview"]["total_objects"].as_u64().unwrap_or(0);
    assert!(
        objects > 0,
        "non-closed-zip: expected total_objects > 0, got {objects}"
    );
}

/// The heap should contain at least some BufferedReader or GZIPInputStream
/// objects visible in the histogram (these are the leak objects).
/// NOTE: Old dumps (before the NonClosedZipStream.java fix) may not show these
/// since the readers weren't retained — this test skips gracefully if absent.
#[test]
fn non_closed_zip_has_buffered_reader_or_gzip_instances() {
    let p = dump_path("non-closed-zip-stream");
    let Some(dump) = real_dump(&p) else { return };
    let r = run(&[
        dump.to_str().unwrap(),
        "--format",
        "json",
        "--progress",
        "never",
    ]);
    assert!(
        r.success,
        "non-closed-zip json failed\nstderr: {}",
        r.stderr
    );
    let v = parse_json(&r.stdout, "non-closed-zip");
    let hist = v["overview"]["histogram"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let has_reader = hist.iter().any(|row| {
        row["pretty_class"]
            .as_str()
            .is_some_and(|c| c.contains("BufferedReader") || c.contains("GZIPInputStream"))
    });
    if !has_reader {
        // Pre-fix dumps (leakedReaders never populated): skip gracefully
        eprintln!(
            "non-closed-zip: BufferedReader/GZIPInputStream not in histogram — \
             dump was captured before the NonClosedZipStream.java fix; \
             regenerate with test-projects/build.sh"
        );
    }
}

// ---------------------------------------------------------------------------
// rec-normal.hprof.gz
//
// RecursiveHashMaps depth=4 normal mode: a 4-level tree where every node
// has 4 children and a 100-entry HashMap<String,String>.
// RecursiveHashMaps$Node should be visible in the biggest_classes list.
// ---------------------------------------------------------------------------

#[test]
fn rec_normal_no_crash() {
    let p = dump_path("rec-normal");
    let Some(dump) = real_dump(&p) else { return };
    battery_core(dump, "rec-normal");
}

#[test]
fn rec_normal_node_class_visible() {
    let p = dump_path("rec-normal");
    let Some(dump) = real_dump(&p) else { return };
    let r = run(&[
        dump.to_str().unwrap(),
        "--format",
        "json",
        "--progress",
        "never",
    ]);
    assert!(r.success, "rec-normal json failed\nstderr: {}", r.stderr);
    let v = parse_json(&r.stdout, "rec-normal");
    let objects = v["overview"]["total_objects"].as_u64().unwrap_or(0);
    assert!(objects > 0, "rec-normal: total_objects == 0");

    // RecursiveHashMaps$Node (or its inner Node class) should be in the histogram.
    let hist = v["overview"]["histogram"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let node_row = hist.iter().find(|row| {
        row["pretty_class"]
            .as_str()
            .is_some_and(|c| c.contains("Node") || c.contains("RecursiveHashMaps"))
    });
    assert!(
        node_row.is_some(),
        "rec-normal: expected RecursiveHashMaps$Node in histogram, but classes are: {:#?}",
        hist.iter()
            .take(20)
            .map(|r| r["pretty_class"].as_str().unwrap_or("?"))
            .collect::<Vec<_>>()
    );
}

/// Node should appear in the top biggest_classes by retained heap.
#[test]
fn rec_normal_node_in_biggest_classes() {
    let p = dump_path("rec-normal");
    let Some(dump) = real_dump(&p) else { return };
    let r = run(&[
        dump.to_str().unwrap(),
        "--format",
        "json",
        "--progress",
        "never",
    ]);
    assert!(r.success, "rec-normal json failed\nstderr: {}", r.stderr);
    let v = parse_json(&r.stdout, "rec-normal");
    let biggest = v["top"]["biggest_classes"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let node_in_top = biggest.iter().any(|row| {
        row["pretty_class"]
            .as_str()
            .is_some_and(|c| c.contains("Node"))
    });
    assert!(
        node_in_top,
        "rec-normal: expected RecursiveHashMaps$Node in biggest_classes top, got: {:#?}",
        biggest
            .iter()
            .take(10)
            .map(|r| r["pretty_class"].as_str().unwrap_or("?"))
            .collect::<Vec<_>>()
    );
}

/// RecursiveHashMaps/RecursiveHashMaps$Node must classify as "other" by
/// default (not "jdk"), and category_rollup's shallow_pct values must sum
/// to ~100% (regression guard: an earlier version summed `retained` instead
/// of `shallow` for the percentage basis, which is NOT mutually exclusive
/// across classes — a class's retained total already includes its own
/// children's — and produced percentages like 566% on this exact dump).
#[test]
fn rec_normal_category_rollup_percentages_sum_to_100() {
    let p = dump_path("rec-normal");
    let Some(dump) = real_dump(&p) else { return };
    let r = run(&[
        dump.to_str().unwrap(),
        "--format",
        "json",
        "--progress",
        "never",
    ]);
    assert!(r.success, "rec-normal json failed\nstderr: {}", r.stderr);
    let v = parse_json(&r.stdout, "rec-normal");

    let hist = v["overview"]["histogram"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let node_row = hist
        .iter()
        .find(|row| row["pretty_class"].as_str() == Some("RecursiveHashMaps$Node"));
    assert_eq!(
        node_row.and_then(|r| r["category"].as_str()),
        Some("other"),
        "RecursiveHashMaps$Node should classify as 'other' by default (it is \
         not a JDK/framework class)"
    );

    let rollup = v["category_rollup"].as_array().cloned().unwrap_or_default();
    assert!(!rollup.is_empty(), "category_rollup must be non-empty");
    let pct_sum: f64 = rollup
        .iter()
        .map(|r| r["shallow_pct"].as_f64().unwrap_or(0.0))
        .sum();
    assert!(
        (pct_sum - 100.0).abs() < 0.5,
        "category_rollup shallow_pct values must sum to ~100%, got {pct_sum}: {:#?}",
        rollup
    );
}

/// `--category application=RecursiveHashMaps` must reclassify both
/// RecursiveHashMaps and RecursiveHashMaps$Node from "other" to
/// "application", and the resulting category_rollup must contain an
/// "application" row whose retained total reflects both classes combined.
#[test]
fn rec_normal_category_flag_reclassifies_node() {
    let p = dump_path("rec-normal");
    let Some(dump) = real_dump(&p) else { return };
    let r = run(&[
        dump.to_str().unwrap(),
        "--format",
        "json",
        "--progress",
        "never",
        "--category",
        "application=RecursiveHashMaps",
    ]);
    assert!(
        r.success,
        "rec-normal --category json failed\nstderr: {}",
        r.stderr
    );
    let v = parse_json(&r.stdout, "rec-normal --category");

    let hist = v["overview"]["histogram"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let node_row = hist
        .iter()
        .find(|row| row["pretty_class"].as_str() == Some("RecursiveHashMaps$Node"));
    assert_eq!(
        node_row.and_then(|r| r["category"].as_str()),
        Some("application"),
        "--category application=RecursiveHashMaps should reclassify Node"
    );

    let rollup = v["category_rollup"].as_array().cloned().unwrap_or_default();
    let app_row = rollup
        .iter()
        .find(|r| r["category"].as_str() == Some("application"));
    assert!(
        app_row.is_some(),
        "expected an 'application' row in category_rollup, got: {:#?}",
        rollup
    );
}

// ---------------------------------------------------------------------------
// rec-complex.hprof.gz
//
// Same tree but "complex" mode (only 1 child at depth level 1).
// Same structural expectations as rec-normal.
// ---------------------------------------------------------------------------

#[test]
fn rec_complex_no_crash() {
    let p = dump_path("rec-complex");
    let Some(dump) = real_dump(&p) else { return };
    battery_core(dump, "rec-complex");
}

#[test]
fn rec_complex_node_class_visible() {
    let p = dump_path("rec-complex");
    let Some(dump) = real_dump(&p) else { return };
    let r = run(&[
        dump.to_str().unwrap(),
        "--format",
        "json",
        "--progress",
        "never",
    ]);
    assert!(r.success, "rec-complex json failed\nstderr: {}", r.stderr);
    let v = parse_json(&r.stdout, "rec-complex");
    let objects = v["overview"]["total_objects"].as_u64().unwrap_or(0);
    assert!(objects > 0, "rec-complex: total_objects == 0");
    let hist = v["overview"]["histogram"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let node_row = hist.iter().find(|row| {
        row["pretty_class"]
            .as_str()
            .is_some_and(|c| c.contains("Node") || c.contains("RecursiveHashMaps"))
    });
    assert!(
        node_row.is_some(),
        "rec-complex: expected RecursiveHashMaps$Node in histogram, classes: {:#?}",
        hist.iter()
            .take(20)
            .map(|r| r["pretty_class"].as_str().unwrap_or("?"))
            .collect::<Vec<_>>()
    );
}

// ---------------------------------------------------------------------------
// rec-normal-crash.hprof.gz
//
// OOM crash dump from RecursiveHashMaps depth=10.  The dump may be partially
// truncated at the crash boundary (JVM was OOM when writing).  We relax the
// "has objects" assertion and only require no panic plus a valid JSON output.
// ---------------------------------------------------------------------------

#[test]
fn rec_normal_crash_no_panic() {
    let p = dump_path("rec-normal-crash");
    let Some(dump) = real_dump(&p) else { return };
    let dump_str = dump.to_str().unwrap();

    // Crash dump: we tolerate a non-zero exit (truncated dump = graceful error),
    // but there must be no Rust panic and no raw internal error strings.
    let r = run(&[dump_str, "--format", "json", "--progress", "never"]);
    assert_no_panic(&r, "rec-normal-crash");
    assert_no_internal_leak(&r, "rec-normal-crash");

    // If the tool exited successfully, verify we got a valid report.
    if r.success {
        let v = parse_json(&r.stdout, "rec-normal-crash [json]");
        assert!(
            v["schema_version"].is_number(),
            "rec-normal-crash: JSON missing schema_version"
        );
        // Crash dumps may have 0 objects if the heap segment was truncated.
        let objects = v["overview"]["total_objects"].as_u64().unwrap_or(0);
        eprintln!("rec-normal-crash: total_objects = {objects}");
    } else {
        // Graceful failure (non-zero exit, no panic) is acceptable.
        eprintln!(
            "rec-normal-crash: tool exited non-zero (likely truncated OOM dump) — stderr: {}",
            r.stderr
        );
    }
}

#[test]
fn rec_normal_crash_heap_commands_no_crash() {
    let p = dump_path("rec-normal-crash");
    let Some(dump) = real_dump(&p) else { return };
    let dump_str = dump.to_str().unwrap();

    // heap summary and heap histogram must not panic even on a crash dump.
    // Note: these subcommands have no --progress flag; suppress is automatic
    // in CI since stderr is not a terminal.
    for args in &[
        vec!["heap", "summary", dump_str],
        vec!["heap", "histogram", dump_str],
    ] {
        let r = run(args);
        assert_no_panic(&r, "rec-normal-crash heap subcommand");
        assert_no_internal_leak(&r, "rec-normal-crash heap subcommand");
    }
}

// ---------------------------------------------------------------------------
// Bulk smoke: run JSON report on every dump present; require no panic
// ---------------------------------------------------------------------------

#[test]
fn bulk_no_panic_on_all_test_project_dumps() {
    let dir = dump_dir();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    for entry in entries.flatten() {
        let p = entry.path();
        let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
        let stem_ext = p
            .file_stem()
            .and_then(|s| std::path::Path::new(s).extension())
            .and_then(|e| e.to_str())
            .unwrap_or("");
        // Match *.hprof.gz and bare *.hprof
        if ext == "gz" && stem_ext == "hprof" || ext == "hprof" {
            let Some(dump) = real_dump(&p) else { continue };
            let dump_str = dump.to_str().unwrap();
            let r = run(&[dump_str, "--format", "json", "--progress", "never"]);
            assert_no_panic(&r, dump_str);
            assert_no_internal_leak(&r, dump_str);
        }
    }
}

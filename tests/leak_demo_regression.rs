/// Regression tests for the leak-demo heap dumps.
///
/// These tests are **opt-in**: they are skipped automatically if the dump
/// files don't exist yet (run `leak-demos/capture_all.sh` first).
///
/// What each test checks:
///   1. Exit code is 0 (no hard crash).
///   2. No Rust panic in stderr ("panicked at").
///   3. No raw internal reader strings leaked into stderr.
///   4. JSON output is syntactically valid and has `schema_version`.
///   5. JSON output contains at least one object (`overview.total_objects > 0`).
///   6. For OOM/leak scenarios (01–17): the Markdown report contains a
///      "Leak Suspects" section.
///   7. For normal-app scenarios (31–34): `total_objects > 0` and the tool
///      does NOT exit non-zero (i.e. it handles healthy dumps gracefully).
///
/// Run:
///   # First generate dumps:
///   cd leak-demos && ./capture_all.sh
///
///   # Then run these tests (from repo root):
///   cargo test --test leak_demo_regression
///
///   # To run a single scenario:
///   cargo test --test leak_demo_regression scenario_01
use std::path::{Path, PathBuf};
use std::process::Command;

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

fn dump_dir() -> PathBuf {
    let root = std::env::var("CARGO_MANIFEST_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."));
    root.join("leak-demos").join("dumps")
}

fn dump_path(name: &str) -> PathBuf {
    dump_dir().join(format!("{name}.hprof"))
}

/// Returns `None` if the file does not exist or is too small to be a real dump
/// (e.g. an LFS pointer stub).  Callers use this to skip tests gracefully.
fn real_dump(path: &Path) -> Option<&Path> {
    match std::fs::metadata(path) {
        Ok(m) if m.len() >= 1024 => Some(path),
        _ => None,
    }
}

fn bin() -> String {
    // CARGO_BIN_EXE_hprof-analyzer is set by Cargo when running integration tests.
    // Fall back to the release binary for ad-hoc runs.
    std::env::var("CARGO_BIN_EXE_hprof-analyzer")
        .unwrap_or_else(|_| "target/release/hprof-analyzer".to_string())
}

// ---------------------------------------------------------------------------
// Command runners
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Assertions
// ---------------------------------------------------------------------------

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

fn assert_json_valid(json: &str, label: &str) -> serde_json::Value {
    serde_json::from_str(json)
        .unwrap_or_else(|e| panic!("{label}: stdout is not valid JSON: {e}\n{json}"))
}

fn assert_json_has_content(v: &serde_json::Value, label: &str) {
    assert!(
        v["schema_version"].is_number(),
        "{label}: JSON missing schema_version"
    );
    let objects = v["overview"]["total_objects"].as_u64().unwrap_or(0);
    assert!(objects > 0, "{label}: overview.total_objects == 0");
}

fn assert_md_has_section(md: &str, section: &str, label: &str) {
    assert!(
        md.contains(section),
        "{label}: Markdown output missing section '{section}'"
    );
}

// ---------------------------------------------------------------------------
// Core test battery (called by every per-scenario test)
// ---------------------------------------------------------------------------

/// Check: no crash, no panic, valid JSON with content.
fn battery_core(dump: &Path, scenario_name: &str) {
    let dump_str = dump.to_str().unwrap();

    // ── 1. JSON report ──
    let r = run(&[dump_str, "--format", "json", "--progress", "never"]);
    assert!(
        r.success,
        "{scenario_name} [json]: exit non-zero\nstderr: {}",
        r.stderr
    );
    assert_no_panic(&r, &format!("{scenario_name} [json]"));
    assert_no_internal_leak(&r, &format!("{scenario_name} [json]"));
    let v = assert_json_valid(&r.stdout, &format!("{scenario_name} [json]"));
    assert_json_has_content(&v, &format!("{scenario_name} [json]"));

    // ── 2. heap summary ──
    // Note: `heap summary` has no --progress flag; the load() closure inside
    // run_heap_cmd reads from the cache built above and suppresses progress
    // automatically when stderr is not a terminal (always the case in CI).
    let r = run(&["heap", "summary", dump_str]);
    assert!(
        r.success,
        "{scenario_name} [summary]: exit non-zero\nstderr: {}",
        r.stderr
    );
    assert_no_panic(&r, &format!("{scenario_name} [summary]"));
    assert_no_internal_leak(&r, &format!("{scenario_name} [summary]"));

    // ── 3. triage section ──
    let r = run(&["heap", "report", dump_str, "--section", "triage"]);
    assert!(
        r.success,
        "{scenario_name} [triage]: exit non-zero\nstderr: {}",
        r.stderr
    );
    assert_no_panic(&r, &format!("{scenario_name} [triage]"));
    assert_no_internal_leak(&r, &format!("{scenario_name} [triage]"));

    // ── 4. leaks section ──
    let r = run(&["heap", "report", dump_str, "--section", "leaks"]);
    assert!(
        r.success,
        "{scenario_name} [leaks]: exit non-zero\nstderr: {}",
        r.stderr
    );
    assert_no_panic(&r, &format!("{scenario_name} [leaks]"));
    assert_no_internal_leak(&r, &format!("{scenario_name} [leaks]"));

    // ── 5. histogram ──
    let r = run(&["heap", "histogram", dump_str]);
    assert!(
        r.success,
        "{scenario_name} [histogram]: exit non-zero\nstderr: {}",
        r.stderr
    );
    assert_no_panic(&r, &format!("{scenario_name} [histogram]"));
    assert_no_internal_leak(&r, &format!("{scenario_name} [histogram]"));
}

/// Additional check for OOM/leak scenarios: the report must flag a Leak Suspect.
fn assert_has_leak_suspect(dump: &Path, scenario_name: &str) {
    let dump_str = dump.to_str().unwrap();
    let r = run(&[dump_str, "--format", "md", "--progress", "never"]);
    assert_no_panic(&r, &format!("{scenario_name} [md-leak-check]"));
    assert_md_has_section(
        &r.stdout,
        "Leak Suspects",
        &format!("{scenario_name} [md-leak-check]"),
    );
}

// ---------------------------------------------------------------------------
// Per-scenario tests
// ---------------------------------------------------------------------------

macro_rules! leak_test {
    ($fn_name:ident, $file_stem:literal, leak) => {
        #[test]
        fn $fn_name() {
            let p = dump_path($file_stem);
            let Some(dump) = real_dump(&p) else { return };
            battery_core(dump, $file_stem);
            assert_has_leak_suspect(dump, $file_stem);
        }
    };
    ($fn_name:ident, $file_stem:literal, normal) => {
        #[test]
        fn $fn_name() {
            let p = dump_path($file_stem);
            let Some(dump) = real_dump(&p) else { return };
            battery_core(dump, $file_stem);
            // Normal apps: just verify no crash — don't require a leak suspect
        }
    };
    ($fn_name:ident, $file_stem:literal, unreachable) => {
        #[test]
        fn $fn_name() {
            let p = dump_path($file_stem);
            let Some(dump) = real_dump(&p) else { return };
            // Unreachable-object dumps may or may not show live leak suspects
            // (depends on capture timing).  We only require no crash.
            let dump_str = dump.to_str().unwrap();
            let r = run(&[dump_str, "--format", "json", "--progress", "never"]);
            assert_no_panic(&r, $file_stem);
            assert_no_internal_leak(&r, $file_stem);
            if r.success {
                let v = assert_json_valid(&r.stdout, $file_stem);
                assert_json_has_content(&v, $file_stem);
            }
            // If exit non-zero but no panic, that's a graceful error — acceptable.
        }
    };
}

// Part 1 — Leaks & OOM
leak_test!(scenario_01, "01_StaticCacheLeak", leak);
leak_test!(scenario_02, "02_ListenerLeak", leak);
leak_test!(scenario_03, "03_InnerClassLeak", leak);
leak_test!(scenario_04, "04_UnboundedCollectionLeak", leak);
leak_test!(scenario_05, "05_ThreadLocalLeak", leak);
leak_test!(scenario_06, "06_ExecutorLeak", leak);
leak_test!(scenario_07, "07_ShutdownHookLeak", leak);
leak_test!(scenario_08, "08_ClassLoaderLeak", leak);
leak_test!(scenario_09, "09_ClassGenerationOOM", leak);
leak_test!(scenario_10, "10_FinalizerQueueLeak", leak);
leak_test!(scenario_11, "11_WeakHashMapLeak", leak);
leak_test!(scenario_12, "12_SoftReferenceCacheLeak", leak);
leak_test!(scenario_13, "13_DirectBufferLeak", leak);
leak_test!(scenario_14, "14_GcOverheadOOM", leak);
leak_test!(scenario_17, "17_PersistenceContextLeak", leak);

// Part 2 — Collection Efficiency Waste
leak_test!(scenario_18, "18_OversizedHashMapWaste", normal);
leak_test!(scenario_19, "19_SparseMapAfterRemoval", normal);
leak_test!(scenario_20, "20_ArrayListCapacityWaste", normal);

// Part 3 — Duplicate Detection
leak_test!(scenario_21, "21_DuplicateStringWaste", normal);
leak_test!(scenario_22, "22_DuplicateByteArrayWaste", normal);

// Part 4 — Object Structure & Overhead
leak_test!(scenario_23, "23_NormalDomainObjectGraph", normal);
leak_test!(scenario_24, "24_MetricTimeSeries", normal);
leak_test!(scenario_25, "25_DeepObjectChain", normal);
leak_test!(scenario_26, "26_BoxedPrimitivesWaste", normal);
leak_test!(scenario_27, "27_ManySmallObjects", normal);

// Part 5 — Edge Cases
leak_test!(scenario_28, "28_StringInternAbuse", normal);
leak_test!(scenario_29, "29_HashMapHighCollisionRate", normal);

// Part 6 — Unreachable Objects
leak_test!(scenario_30, "30_GcOverwhelmedGarbage", unreachable);

// Part 7 — Normal Applications (no bugs)
leak_test!(scenario_31, "31_AppWordFrequency", normal);
leak_test!(scenario_32, "32_AppSessionStore", normal);
leak_test!(scenario_33, "33_AppEventPipeline", normal);
leak_test!(scenario_34, "34_AppProductCatalog", normal);

// Part 8 — New Leak Patterns
leak_test!(scenario_35, "35_ClassLoaderSpam", normal);
leak_test!(scenario_36, "36_ClassLoaderLeak", leak);
leak_test!(scenario_37, "37_ThreadLocalPooledThread", leak);
leak_test!(scenario_38, "38_ExcessiveThreads", normal);
leak_test!(scenario_39, "39_ThreadExecutorLeak", leak);
leak_test!(scenario_40, "40_DirectBufferLeak", leak);

// ---------------------------------------------------------------------------
// Bulk smoke tests (fast): run JSON report on every dump present, no asserts
// beyond "no panic".  Catches scenarios for which no specific test was written.
// ---------------------------------------------------------------------------
#[test]
fn bulk_no_panic_on_all_dumps() {
    let dir = dump_dir();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    for entry in entries.flatten() {
        let p = entry.path();
        if p.extension().is_some_and(|e| e == "hprof") {
            let Some(dump) = real_dump(&p) else { continue };
            let dump_str = dump.to_str().unwrap();
            let r = run(&[dump_str, "--format", "json", "--progress", "never"]);
            assert_no_panic(&r, dump_str);
            assert_no_internal_leak(&r, dump_str);
        }
    }
}

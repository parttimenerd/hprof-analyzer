#![cfg(feature = "mat-oracle")]
//! Opt-in Eclipse MAT (Memory Analyzer) differential-oracle test harness.
//!
//! This test cross-checks our OQL engine against a reference implementation
//! (Eclipse MAT) by running the same query through both and comparing results:
//!   * For object-returning queries it compares object-ADDRESS SETS
//!     (order-independent — see `addr_set` / `compare_superset`).
//!   * For scalar queries (e.g. `COUNT(*)`) it compares the trimmed scalar
//!     strings (`compare_scalar`).
//!
//! It is entirely opt-in:
//!   * The file only compiles under `--features mat-oracle` (the `#![cfg]`
//!     above makes the module empty otherwise, so the default build never
//!     touches it).
//!   * The single test is `#[ignore]`d, so it never runs unless explicitly
//!     requested with `-- --ignored`.
//!   * When `MAT_HOME` is unset it GREEN-SKIPS: it prints a note and returns
//!     early without panicking. This lets CI run it harmlessly.
//!
//! VERIFIED against Eclipse MAT 1.13.0 (2026-07-23, macOS). Point `MAT_HOME`
//! at the Eclipse dir inside the app bundle, e.g.
//! `/Applications/mat.app/Contents/Eclipse`, and `ORACLE_HPROF` at a fixture:
//!   MAT_HOME=/Applications/mat.app/Contents/Eclipse \
//!   ORACLE_HPROF=$PWD/tests/fixtures/dump_4_philosophers.hprof \
//!   cargo test --features mat-oracle --test mat_oracle -- --ignored --nocapture
//!
//! ADDRESS FORMAT: all object queries here project `@objectAddress` so both
//! sides emit comparable DECIMAL addresses (`addr_set` also accepts `0x<hex>`).
//! `SELECT *` is avoided because our binary renders object rows as
//! `class@index`, which is not an address.
//!
//! REACHABILITY DIVERGENCE (systematic, confirmed 2026-07-23): MAT discards
//! UNREACHABLE objects during indexing; our `query` subcommand scans the raw
//! heap and includes them. So for class-pattern queries MAT returns a SUBSET of
//! our rows (MAT ⊆ ours). Equality comparison would therefore false-fail. The
//! harness compares with `compare_superset` (asserts MAT ⊆ ours and reports the
//! count delta) instead of strict set equality. e.g. on dump_4_philosophers,
//! `FROM java.lang.Thread` → MAT 27, ours 29 (2 unreachable Threads); `FROM
//! java.lang.String` → MAT 23331, ours 24760.

use std::collections::BTreeSet;

fn mat_home() -> Option<String> {
    std::env::var("MAT_HOME").ok()
}

fn addr_set(lines: &str) -> BTreeSet<u64> {
    lines
        .lines()
        .filter_map(|l| {
            let t = l.trim().trim_start_matches("0x");
            u64::from_str_radix(t, 16).ok().or_else(|| t.parse().ok())
        })
        .collect()
}
fn normalize_for_mat(oql: &str) -> String {
    oql.to_string() /* hook for dialect tweaks */
}
fn compare_scalar(ours: &str, mat: &str) -> Result<(), String> {
    if ours.trim() == mat.trim() {
        Ok(())
    } else {
        Err(format!("scalar mismatch: ours={ours:?} mat={mat:?}"))
    }
}

/// Assert MAT's address set is a SUBSET of ours (MAT ⊆ ours). MAT drops
/// unreachable objects during indexing, so for class-pattern queries it returns
/// fewer rows than our raw-heap scan. A pass means every MAT-returned object was
/// also found by us; the count delta (ours − mat) is the unreachable-object
/// count and is reported for visibility, not treated as a failure. A FAILURE
/// means MAT returned an object we did NOT — a genuine miss on our side.
fn compare_superset(ours: &BTreeSet<u64>, mat: &BTreeSet<u64>) -> Result<(), String> {
    let mat_only: Vec<_> = mat.difference(ours).take(10).collect();
    if mat_only.is_empty() {
        // MAT ⊆ ours. Report the unreachable delta for visibility.
        let extra = ours.len().saturating_sub(mat.len());
        eprintln!(
            "    MAT ⊆ ours ✓ (|mat|={} |ours|={}, {extra} extra ~= unreachable)",
            mat.len(),
            ours.len(),
        );
        Ok(())
    } else {
        Err(format!(
            "MAT returned objects we MISSED (≤10 shown): {mat_only:?} |mat|={} |ours|={}",
            mat.len(),
            ours.len(),
        ))
    }
}

/// Shell out to `scripts/mat-oracle.sh <hprof> <oql>`, inheriting the current
/// environment (so it sees `MAT_HOME`), and return its stdout.
///
/// This is only ever called after `MAT_HOME` is confirmed set, so any spawn
/// failure or non-zero exit is a genuine error worth surfacing loudly.
fn run_mat(hprof: &str, oql: &str) -> String {
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/scripts/mat-oracle.sh");
    let out = std::process::Command::new(script)
        .args([hprof, oql])
        .output()
        .unwrap_or_else(|e| {
            panic!("failed to spawn MAT oracle script {script:?} for OQL {oql:?}: {e}")
        });
    if !out.status.success() {
        panic!(
            "MAT oracle script {script:?} exited with {status} for OQL {oql:?}\nstderr:\n{stderr}",
            status = out.status,
            stderr = String::from_utf8_lossy(&out.stderr),
        );
    }
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Invoke the built binary in query mode (`query <hprof> --query <oql>`) and
/// return its stdout. The binary is located via the standard cargo integration
/// mechanism (`CARGO_BIN_EXE_*`), so no PATH assumptions are made.
fn run_ours(hprof: &str, oql: &str) -> String {
    let bin = env!("CARGO_BIN_EXE_hprof-analyzer");
    let out = std::process::Command::new(bin)
        .args(["query", hprof, "--query", oql])
        .output()
        .unwrap_or_else(|e| panic!("failed to spawn binary {bin:?} for OQL {oql:?}: {e}"));
    if !out.status.success() {
        panic!(
            "binary {bin:?} exited with {status} for OQL {oql:?}\nstderr:\n{stderr}",
            status = out.status,
            stderr = String::from_utf8_lossy(&out.stderr),
        );
    }
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// (label, oql, kind). `kind` selects the comparison:
///   * `Superset` — object query projecting `@objectAddress`; assert MAT ⊆ ours
///     (MAT drops unreachable objects — see module docs).
///   * `Scalar` — a single scalar value compared as a trimmed string.
///
/// All object queries project `@objectAddress` so both engines emit comparable
/// decimal addresses.
#[derive(Clone, Copy)]
enum Cmp {
    Superset,
    // Scalar comparison (trimmed strings). Not currently used by any oracle
    // query: MAT does not emit CSV for single-scalar results (e.g. COUNT(*)),
    // so scalar queries can't be compared through the CSV report mechanism.
    // Kept for when a scalar-capable export path is wired up.
    #[allow(dead_code)]
    Scalar,
}

const ORACLE_QUERIES: &[(&str, &str, Cmp)] = &[
    (
        "strings",
        "SELECT @objectAddress FROM java.lang.String",
        Cmp::Superset,
    ),
    (
        "threads",
        "SELECT @objectAddress FROM java.lang.Thread",
        Cmp::Superset,
    ),
    (
        "instanceof_thread",
        "SELECT @objectAddress FROM INSTANCEOF java.lang.Thread",
        Cmp::Superset,
    ),
    // NOTE: several query classes can't be carried through MAT's headless
    // CSV-report mechanism and are validated by our own unit/integration tests
    // instead:
    //   1. Double-quote-containing OQL (regex `FROM "..."`, `LIKE "..."`, quoted
    //      aliases) — the inner quote terminates MAT's `oql "<query>"` wrapper.
    //   2. `UNION` — MAT returns a compound IResultTree; its CSV exporter emits
    //      an empty file.
    //   3. `... IN (SELECT ...)` subquery predicates — MAT's headless export of
    //      the result produces no CSV (compound/empty), so it isn't comparable.
    // The oracle only carries flat, single-table, quote-free class queries,
    // which is exactly what exercises the reachability-filtering divergence.
];

#[test]
#[ignore = "requires MAT_HOME and --features mat-oracle"]
fn oracle_differential() {
    let Some(_home) = mat_home() else {
        eprintln!("MAT_HOME unset — skipping");
        return;
    };
    let hprof = std::env::var("ORACLE_HPROF").expect("set ORACLE_HPROF to a dump path");
    let mut failures = Vec::new();
    for (label, oql, kind) in ORACLE_QUERIES {
        eprintln!("[{label}] {oql}");
        let mat_out = run_mat(&hprof, &normalize_for_mat(oql)); // shells scripts/mat-oracle.sh
        let our_out = run_ours(&hprof, oql); // invokes this binary's query mode
        let res = match kind {
            Cmp::Scalar => compare_scalar(&our_out, &mat_out),
            Cmp::Superset => compare_superset(&addr_set(&our_out), &addr_set(&mat_out)),
        };
        if let Err(e) = res {
            failures.push(format!("[{label}] {e}"));
        }
    }
    assert!(failures.is_empty(), "oracle divergences:\n{}", failures.join("\n"));
}

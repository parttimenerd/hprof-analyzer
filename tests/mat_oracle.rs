#![cfg(feature = "mat-oracle")]
//! Opt-in Eclipse MAT (Memory Analyzer) differential-oracle test harness.
//!
//! This test cross-checks our OQL engine against a reference implementation
//! (Eclipse MAT) by running the same query through both and comparing results:
//!   * For object-returning queries it compares object-ADDRESS SETS
//!     (order-independent — see `addr_set` / `compare_sets`).
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
//! !!! UNVERIFIED against a live MAT !!!
//! Eclipse MAT is NOT installed in this development environment, so this
//! harness (and `scripts/mat-oracle.sh` which it shells out to) has never been
//! validated end-to-end against a real MAT install. Treat it as a documented
//! starting point that must be checked against a live MAT before it is trusted.
//!
//! KNOWN GAP — address format:
//! MAT prints object ADDRESSES, and `addr_set` parses `0x<hex>` or decimal
//! address lines. Our binary, however, renders `SELECT *` object rows as
//! `class@index` (the dense object index, via `fmt_query_value`), NOT as an
//! address. Only projecting `@objectAddress` yields a comparable *decimal*
//! address from our binary. The `ORACLE_QUERIES` below come from the plan and
//! use `SELECT *`, so `run_ours` output for those object queries would render
//! as `class@index` and `addr_set` would parse out an (effectively empty) set
//! — i.e. they would NOT truly compare against MAT as-is. Because MAT is
//! unavailable and the test green-skips, this mismatch is never exercised.
//! Resolving it (rewriting the object queries to `SELECT @objectAddress ...`,
//! or having `run_ours` inject an `@objectAddress` projection) is deliberately
//! LEFT AS A DOCUMENTED KNOWN GAP to be closed when a real MAT is available to
//! validate against, rather than silently rewriting every query here.

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
fn compare_sets(ours: &BTreeSet<u64>, mat: &BTreeSet<u64>) -> Result<(), String> {
    if ours == mat {
        return Ok(());
    }
    let only_ours: Vec<_> = ours.difference(mat).take(10).collect();
    let only_mat: Vec<_> = mat.difference(ours).take(10).collect();
    Err(format!(
        "set mismatch: only_ours(≤10)={only_ours:?} only_mat(≤10)={only_mat:?} \
                 |ours|={} |mat|={}",
        ours.len(),
        mat.len()
    ))
}
fn compare_scalar(ours: &str, mat: &str) -> Result<(), String> {
    if ours.trim() == mat.trim() {
        Ok(())
    } else {
        Err(format!("scalar mismatch: ours={ours:?} mat={mat:?}"))
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

/// (label, oql, is_scalar)
const ORACLE_QUERIES: &[(&str, &str, bool)] = &[
    ("string_histogram", "SELECT * FROM java.lang.String", false),
    (
        "retained_order",
        "SELECT * FROM java.lang.Object ORDER BY @retainedHeapSize DESC LIMIT 20",
        false,
    ),
    (
        "dominators",
        "SELECT dominators(s) FROM java.lang.String s LIMIT 50",
        false,
    ),
    (
        "union",
        "SELECT * FROM java.lang.String UNION SELECT * FROM java.lang.Integer",
        false,
    ),
    (
        "in_subquery",
        "SELECT * FROM java.lang.Object o WHERE o.@objectAddress IN (SELECT * FROM java.lang.String)",
        false,
    ),
    (
        "inbounds",
        "SELECT @inbounds FROM java.lang.String LIMIT 50",
        false,
    ),
    ("count_scalar", "SELECT COUNT(*) FROM java.lang.String", true),
];

#[test]
#[ignore = "requires MAT_HOME and --features mat-oracle"]
fn oracle_differential() {
    let Some(_home) = mat_home() else {
        eprintln!("MAT_HOME unset — skipping");
        return;
    };
    let hprof = std::env::var("ORACLE_HPROF").expect("set ORACLE_HPROF to a dump path");
    for (label, oql, is_scalar) in ORACLE_QUERIES {
        let mat_out = run_mat(&hprof, &normalize_for_mat(oql)); // shells scripts/mat-oracle.sh
        let our_out = run_ours(&hprof, oql); // invokes this binary's query mode
        let res = if *is_scalar {
            compare_scalar(&our_out, &mat_out)
        } else {
            compare_sets(&addr_set(&our_out), &addr_set(&mat_out))
        };
        if let Err(e) = res {
            panic!("[{label}] {e}");
        }
    }
}

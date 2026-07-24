//! Oracle and parity tests for the OQL engine.
//!
//! # Eclipse MAT differential oracle (opt-in, feature-gated)
//!
//! This cross-checks our OQL engine against a reference implementation
//! (Eclipse MAT) by running the same query through both and comparing results:
//!   * For object-returning queries it compares object-ADDRESS SETS
//!     (order-independent — see `addr_set` / `compare_superset`).
//!   * For scalar queries (e.g. `COUNT(*)`) it compares the trimmed scalar
//!     strings (`compare_scalar`).
//!
//! The MAT differential oracle is entirely opt-in:
//!   * It only compiles under `--features mat-oracle`.
//!   * The oracle test is `#[ignore]`d, so it never runs unless explicitly
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
//! UNREACHABLE objects during indexing. Our `query` subcommand now DEFAULTS to
//! reachable-only (MAT parity); `--all` opts back into the raw-heap scan that
//! includes unreachable objects. So the harness runs each class-pattern query
//! two ways:
//!   * `--all` (raw heap) vs MAT → `compare_superset` (MAT ⊆ ours; the delta is
//!     the unreachable-object count, reported for visibility).
//!   * reachable-only default vs MAT → `compare_exact` (ours == MAT exactly).
//! e.g. on dump_4_philosophers, `FROM java.lang.Thread` → MAT 27, ours-`--all`
//! 29 (2 unreachable Threads), ours-reachable 27 (== MAT).
//!
//! # Self-contained parity tests (always compiled)
//!
//! `group_by_count_matches_manual_count` runs without MAT or the feature gate.

// ---------------------------------------------------------------------------
// Shared helpers (always compiled)
// ---------------------------------------------------------------------------

/// Invoke the built binary in query mode (`query <hprof> [extra...] --query
/// <oql>`) and return its stdout. The binary is located via the standard cargo
/// integration mechanism (`CARGO_BIN_EXE_*`), so no PATH assumptions are made.
/// `extra` inserts flags (e.g. `--all`) between the dump path and `--query`.
fn run_ours_args(hprof: &str, extra: &[&str], oql: &str) -> String {
    let bin = env!("CARGO_BIN_EXE_hprof-analyzer");
    let mut args: Vec<&str> = vec!["query", hprof];
    args.extend_from_slice(extra);
    args.extend_from_slice(&["--query", oql]);
    let out = std::process::Command::new(bin)
        .args(&args)
        .output()
        .unwrap_or_else(|e| panic!("failed to spawn binary {bin:?} for OQL {oql:?}: {e}"));
    if !out.status.success() {
        panic!(
            "binary {bin:?} exited with {status} for OQL {oql:?} (args {args:?})\nstderr:\n{stderr}",
            status = out.status,
            stderr = String::from_utf8_lossy(&out.stderr),
        );
    }
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Run `query <hprof> --query <oql>` with our binary and return stdout.
/// Panics on non-zero exit so test failures surface the stderr.
fn run_query(hprof: &str, oql: &str) -> String {
    run_ours_args(hprof, &[], oql)
}

/// Locate the committed philosophers dump, or return `None` when it is an
/// unhydrated LFS pointer (so CI without LFS still passes).
fn philosophers_hprof() -> Option<String> {
    let p = format!(
        "{}/tests/fixtures/dump_4_philosophers.hprof",
        env!("CARGO_MANIFEST_DIR")
    );
    match std::fs::metadata(&p) {
        Ok(m) if m.len() >= 1024 => Some(p),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Self-contained parity tests (no MAT, no feature gate)
// ---------------------------------------------------------------------------

#[test]
fn group_by_count_matches_manual_count() {
    let Some(hprof) = philosophers_hprof() else {
        return;
    };
    // GROUP BY COUNT(*) for a class should equal direct COUNT(*) FROM <class>
    let grouped = run_query(
        &hprof,
        "SELECT @displayName, COUNT(*) AS n FROM java.lang.String GROUP BY @displayName",
    );
    let direct = run_query(&hprof, "SELECT COUNT(*) FROM java.lang.String");
    // Extract count from grouped output (find the String row, last pipe-separated column)
    let grouped_n: u64 = grouped
        .lines()
        .find(|l| l.contains("String") && l.contains('|'))
        .and_then(|l| l.split('|').last())
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0);
    // Extract count from direct query (the line that is a bare integer)
    let direct_n: u64 = direct
        .lines()
        .find_map(|l| l.trim().parse::<u64>().ok())
        .unwrap_or(1);
    assert_eq!(
        grouped_n, direct_n,
        "GROUP BY count must match direct COUNT(*)\ngrouped:\n{grouped}\ndirect:\n{direct}"
    );
}

// ---------------------------------------------------------------------------
// Eclipse MAT differential oracle (requires --features mat-oracle)
// ---------------------------------------------------------------------------

#[cfg(feature = "mat-oracle")]
mod mat_oracle {
    use super::{philosophers_hprof, run_ours_args};
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

    #[allow(dead_code)]
    fn compare_scalar(ours: &str, mat: &str) -> Result<(), String> {
        if ours.trim() == mat.trim() {
            Ok(())
        } else {
            Err(format!("scalar mismatch: ours={ours:?} mat={mat:?}"))
        }
    }

    /// Assert MAT's address set is a SUBSET of ours (MAT ⊆ ours). MAT drops
    /// unreachable objects during indexing, so for a raw-heap (`--all`) scan it
    /// returns fewer rows than us. A pass means every MAT-returned object was also
    /// found by us; the count delta (ours − mat) is the unreachable-object count and
    /// is reported for visibility, not treated as a failure. A FAILURE means MAT
    /// returned an object we did NOT — a genuine miss on our side.
    ///
    /// Feed this the `--all` (raw-heap) output; the reachable-only default would
    /// make ours == mat, for which `compare_exact` is the sharper check.
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

    /// Assert our address set EQUALS MAT's exactly (ours == mat). This is the sharp
    /// MAT-parity check for the reachable-only default: we filter OQL results to
    /// GC-reachable objects, so class-pattern queries should now match MAT
    /// object-for-object with no unreachable delta. Feed this the reachable-only
    /// (default) output. Reports BOTH directions of the diff on failure:
    ///   * `mat_only` — objects MAT returned that we dropped (over-pruned).
    ///   * `ours_only` — objects we returned that MAT dropped (under-pruned; the
    ///     reachability delta the reachable-only default was meant to eliminate).
    fn compare_exact(ours: &BTreeSet<u64>, mat: &BTreeSet<u64>) -> Result<(), String> {
        let mat_only: Vec<_> = mat.difference(ours).take(10).collect();
        let ours_only: Vec<_> = ours.difference(mat).take(10).collect();
        if mat_only.is_empty() && ours_only.is_empty() {
            eprintln!("    ours == MAT ✓ (|both|={})", mat.len());
            Ok(())
        } else {
            Err(format!(
                "exact-parity mismatch: mat_only(≤10)={mat_only:?} ours_only(≤10)={ours_only:?} |mat|={} |ours|={}",
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

    /// Reachable-only (the query-subcommand DEFAULT — MAT parity). Compared with
    /// `compare_exact`.
    fn run_ours_reachable(hprof: &str, oql: &str) -> String {
        run_ours_args(hprof, &[], oql)
    }

    /// Raw-heap scan (`--all`) — includes unreachable objects. Compared with
    /// `compare_superset` (MAT ⊆ ours).
    fn run_ours_all(hprof: &str, oql: &str) -> String {
        run_ours_args(hprof, &["--all"], oql)
    }

    /// (label, oql, kind). `kind` selects the comparison AND which of our two scan
    /// modes to invoke:
    ///   * `Superset` — object query projecting `@objectAddress`, run with `--all`
    ///     (raw heap); assert MAT ⊆ ours (MAT drops unreachable objects).
    ///   * `Exact` — object query projecting `@objectAddress`, run reachable-only
    ///     (the default); assert ours == MAT exactly (sharp MAT-parity check).
    ///   * `Scalar` — a single scalar value compared as a trimmed string.
    ///
    /// All object queries project `@objectAddress` so both engines emit comparable
    /// decimal addresses.
    #[derive(Clone, Copy)]
    enum Cmp {
        Superset,
        Exact,
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
        // Exact-equality (reachable-only default == MAT). These re-run the same
        // class patterns but as the sharp parity check: after GC-reachability
        // filtering ours should match MAT object-for-object, delta 0.
        (
            "threads_exact",
            "SELECT @objectAddress FROM java.lang.Thread",
            Cmp::Exact,
        ),
        (
            "instanceof_thread_exact",
            "SELECT @objectAddress FROM INSTANCEOF java.lang.Thread",
            Cmp::Exact,
        ),
        (
            "hashmap_exact",
            "SELECT @objectAddress FROM java.util.HashMap",
            Cmp::Exact,
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
            let res = match kind {
                // Scalar compares our default (reachable-only) stdout as a string.
                Cmp::Scalar => compare_scalar(&run_ours_reachable(&hprof, oql), &mat_out),
                // Superset uses the raw-heap (--all) scan: MAT ⊆ ours.
                Cmp::Superset => {
                    compare_superset(&addr_set(&run_ours_all(&hprof, oql)), &addr_set(&mat_out))
                }
                // Exact uses the reachable-only default: ours == MAT.
                Cmp::Exact => {
                    compare_exact(&addr_set(&run_ours_reachable(&hprof, oql)), &addr_set(&mat_out))
                }
            };
            if let Err(e) = res {
                failures.push(format!("[{label}] {e}"));
            }
        }
        assert!(failures.is_empty(), "oracle divergences:\n{}", failures.join("\n"));
    }
}

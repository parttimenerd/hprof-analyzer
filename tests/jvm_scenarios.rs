//! JVM-scenario regression tests — verify that real heap dumps from `leak-demos/`
//! trigger the expected triage signals.
//!
//! # Gating
//!
//! These tests are skipped unless the `JVM_SCENARIOS_DIR` environment variable
//! points to a directory containing the captured `.hprof` files.  The expected
//! layout is:
//!
//! ```text
//! $JVM_SCENARIOS_DIR/
//!   05_ThreadLocalLeak.hprof
//!   36_ClassLoaderLeak.hprof
//!   ...
//! ```
//!
//! Set the variable and run with:
//! ```
//! JVM_SCENARIOS_DIR=leak-demos/dumps cargo test --test jvm_scenarios
//! ```
//!
//! # Annotations
//!
//! Each `leak-demos/*.java` file that should be tested must contain a line in its
//! opening doc-comment of the form:
//! ```
//!  * expected-triage: <signal-id>
//! ```
//! Optionally, also:
//! ```
//!  * expected-culprit: <class-name-substring>
//! ```
//!
//! The harness discovers all annotated files at test time, finds the matching
//! `.hprof` by replacing the `.java` stem in `$JVM_SCENARIOS_DIR`, and asserts
//! that the named signal appears in `report.triage` and (if present) that
//! `primary_culprits[0].class_name` contains the culprit substring.

use hprof_analyzer::report::Report;
use hprof_analyzer::{AnalyzeOptions, HprofSource};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Annotation parser
// ---------------------------------------------------------------------------

struct JavaAnnotations {
    /// Java source file name without extension, e.g. "36_ClassLoaderLeak"
    stem: String,
    /// Value of `expected-triage: <id>` line
    expected_triage: String,
    /// Value of `expected-culprit: <class>` line, if present
    expected_culprit: Option<String>,
}

fn parse_annotations(java_path: &Path) -> Option<JavaAnnotations> {
    let src = fs::read_to_string(java_path).ok()?;
    let stem = java_path.file_stem()?.to_string_lossy().into_owned();

    let expected_triage = src.lines().find_map(|l| {
        let t = l.trim().trim_start_matches('*').trim();
        t.strip_prefix("expected-triage:")
            .map(|v| v.trim().to_owned())
    })?;

    let expected_culprit = src.lines().find_map(|l| {
        let t = l.trim().trim_start_matches('*').trim();
        t.strip_prefix("expected-culprit:")
            .map(|v| v.trim().to_owned())
    });

    Some(JavaAnnotations {
        stem,
        expected_triage,
        expected_culprit,
    })
}

// ---------------------------------------------------------------------------
// Runner
// ---------------------------------------------------------------------------

fn run_jvm_scenario(dumps_dir: &Path, ann: &JavaAnnotations) {
    let hprof_path = dumps_dir.join(format!("{}.hprof", ann.stem));
    if !hprof_path.exists() {
        eprintln!(
            "[jvm_scenarios] SKIP {} — dump not found at {}",
            ann.stem,
            hprof_path.display()
        );
        return;
    }

    let source = HprofSource::Path(hprof_path.to_string_lossy().into_owned());
    let opts = AnalyzeOptions::default();

    let (report, _retained) = hprof_analyzer::analyze_to_report_with_retained(&source, &opts)
        .unwrap_or_else(|e| {
            panic!("jvm scenario {:?}: analysis failed: {e}", ann.stem);
        });

    assert_triage_fired(&report, &ann.stem, &ann.expected_triage);

    if let Some(culprit) = &ann.expected_culprit {
        assert_culprit_named(&report, &ann.stem, culprit);
    }
}

fn assert_triage_fired(report: &Report, stem: &str, signal_id: &str) {
    let found = report.triage.iter().any(|s| s.id == signal_id);
    assert!(
        found,
        "jvm scenario {stem:?}: expected triage signal {signal_id:?} to fire, \
         but it did not.\n\
         Signals that fired: {:#?}",
        report.triage.iter().map(|s| &s.id).collect::<Vec<_>>(),
    );
}

fn assert_culprit_named(report: &Report, stem: &str, class_contains: &str) {
    assert!(
        !report.primary_culprits.is_empty(),
        "jvm scenario {stem:?}: expected primary culprit containing {class_contains:?}, \
         but primary_culprits is empty",
    );
    let got = &report.primary_culprits[0].class_name;
    assert!(
        got.contains(class_contains),
        "jvm scenario {stem:?}: primary_culprits[0].class_name = {got:?}, \
         expected it to contain {class_contains:?}",
    );
}

// ---------------------------------------------------------------------------
// Discovery + test entry point
// ---------------------------------------------------------------------------

fn leak_demos_dir() -> PathBuf {
    // Resolve relative to Cargo.toml (project root).
    let manifest = env!("CARGO_MANIFEST_DIR");
    Path::new(manifest).join("leak-demos")
}

#[test]
fn jvm_scenarios() {
    let dumps_dir = match env::var("JVM_SCENARIOS_DIR") {
        Ok(v) => PathBuf::from(v),
        Err(_) => {
            eprintln!("[jvm_scenarios] SKIP — JVM_SCENARIOS_DIR not set");
            return;
        }
    };

    let demos = leak_demos_dir();
    let mut java_files: Vec<_> = fs::read_dir(&demos)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", demos.display()))
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("java"))
        .collect();
    java_files.sort();

    let mut ran = 0usize;
    let mut skipped_no_dump = 0usize;

    for java_path in &java_files {
        let Some(ann) = parse_annotations(java_path) else {
            continue;
        };
        let hprof_path = dumps_dir.join(format!("{}.hprof", ann.stem));
        if !hprof_path.exists() {
            eprintln!("[jvm_scenarios] SKIP {} — dump not found", ann.stem);
            skipped_no_dump += 1;
            continue;
        }
        eprintln!(
            "[jvm_scenarios] RUN  {} (expected-triage: {})",
            ann.stem, ann.expected_triage
        );
        run_jvm_scenario(&dumps_dir, &ann);
        ran += 1;
    }

    if ran == 0 && skipped_no_dump == 0 {
        panic!(
            "JVM_SCENARIOS_DIR is set to {:?} but no annotated Java files were found \
             in {} — did you forget to annotate the demos?",
            dumps_dir,
            demos.display()
        );
    }

    eprintln!("[jvm_scenarios] DONE — ran {ran} scenario(s), skipped {skipped_no_dump} (no dump)");
}

// The structural Markdown helpers live in `src/md_test.rs`, gated behind
// `#[cfg(test)]` in the binary crate — which an integration test (a separate
// crate) cannot import. Rather than add a dependency or duplicate the code, we
// `#[path]`-include the same source file here so both places share one parser.
#[path = "../src/md_test.rs"]
mod md_test;
use md_test::Md;

#[test]
fn end_to_end_dump0() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/dump_0_fj-kmeans.hprof"
    );
    // Skip if the LFS fixture is absent or an unsmudged pointer (CI runs `git lfs pull`).
    match std::fs::metadata(path) {
        Ok(m) if m.len() >= 1024 => {}
        _ => return,
    }
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_hprof-analyzer"))
        .arg("analyze")
        .arg(path)
        .output()
        .expect("failed to run hprof-analyzer");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let md = String::from_utf8_lossy(&out.stdout);
    let doc = Md::parse(&md);

    // Major sections are H2.
    assert_eq!(
        doc.heading("System Overview").map(|h| h.level()),
        Some(2),
        "missing System Overview (H2)"
    );
    assert_eq!(
        doc.heading("Leak Suspects").map(|h| h.level()),
        Some(2),
        "missing Leak Suspects (H2)"
    );
    assert_eq!(
        doc.heading("Top Consumers").map(|h| h.level()),
        Some(2),
        "missing Top Consumers (H2)"
    );

    // Sub-sections are H3.
    assert_eq!(
        doc.heading("Heap Summary").map(|h| h.level()),
        Some(3),
        "missing Heap Summary (H3)"
    );
    assert_eq!(
        doc.heading("Class Histogram").map(|h| h.level()),
        Some(3),
        "missing Class Histogram (H3)"
    );
    assert_eq!(
        doc.heading("Biggest Objects").map(|h| h.level()),
        Some(3),
        "missing Biggest Objects (H3)"
    );
    assert_eq!(
        doc.heading("Biggest Classes").map(|h| h.level()),
        Some(3),
        "missing Biggest Classes (H3)"
    );
    assert_eq!(
        doc.heading("Biggest Packages").map(|h| h.level()),
        Some(3),
        "missing Biggest Packages (H3)"
    );

    // Structural nesting: Heap Summary and Class Histogram live inside System
    // Overview's body, and the histogram is a real table with a Class column.
    let sys = doc.section("System Overview").unwrap();
    assert!(
        sys.body_contains("### Heap Summary"),
        "Heap Summary should be nested under System Overview"
    );
    let hist = doc
        .section("Class Histogram")
        .expect("Class Histogram section");
    let table = hist.table(0).expect("Class Histogram renders a table");
    assert!(
        table.has_column("Class"),
        "histogram table should have a Class column, got {:?}",
        table.columns()
    );
    assert!(
        table.has_column("Retained Heap"),
        "histogram table should have a Retained Heap column"
    );
}

/// Blank out the two report fields that legitimately vary between runs, so the
/// rest of the JSON can be compared byte-for-byte against the golden fixture.
/// `generated` is a per-run UTC timestamp; `overview.file_path` echoes the CLI
/// path argument, which is absolute (via `CARGO_MANIFEST_DIR`) in the test but
/// relative in the golden. Everything else (including `source_name`, a
/// basename) is deterministic.
fn normalize_nondeterministic(v: &mut serde_json::Value) {
    if let Some(obj) = v.as_object_mut() {
        if obj.contains_key("generated") {
            obj["generated"] = serde_json::Value::Null;
        }
        if let Some(ov) = obj.get_mut("overview").and_then(|o| o.as_object_mut()) {
            if ov.contains_key("file_path") {
                ov["file_path"] = serde_json::Value::Null;
            }
        }
    }
}

/// End-to-end golden snapshot: a fresh JSON run must equal the committed golden
/// report (modulo the two run-varying fields). This catches ANY unintended
/// change to the emitted model — a new/removed field, a reordered list, a
/// changed count — that the structural assertions above would miss.
#[test]
fn json_golden_snapshot() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");
    let hprof = format!("{dir}/dump_4_philosophers.hprof");
    let golden_path = format!("{dir}/dump_4_philosophers_report.json");

    // Skip if the LFS fixture is absent or an unsmudged pointer (CI runs `git lfs pull`).
    match std::fs::metadata(&hprof) {
        Ok(m) if m.len() >= 1024 => {}
        _ => return,
    }

    let out = std::process::Command::new(env!("CARGO_BIN_EXE_hprof-analyzer"))
        .arg("analyze")
        .arg(&hprof)
        .arg("--format")
        .arg("json")
        .arg("--compress")
        .arg("none")
        .output()
        .expect("failed to run hprof-analyzer");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let mut got: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("analyzer stdout was not valid JSON");
    let golden_text = std::fs::read_to_string(&golden_path)
        .unwrap_or_else(|e| panic!("cannot read golden {golden_path}: {e}"));
    let mut want: serde_json::Value =
        serde_json::from_str(&golden_text).expect("golden fixture was not valid JSON");

    normalize_nondeterministic(&mut got);
    normalize_nondeterministic(&mut want);

    assert_eq!(
        got, want,
        "JSON report drifted from the golden snapshot at {golden_path}. If this \
         change is intended, regenerate the golden with:\n  \
         cargo run --release -- analyze tests/fixtures/dump_4_philosophers.hprof \
         --format json --compress none > tests/fixtures/dump_4_philosophers_report.json"
    );
}

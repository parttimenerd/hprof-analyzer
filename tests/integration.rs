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

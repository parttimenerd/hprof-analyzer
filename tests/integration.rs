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
    assert!(md.contains("## System Overview"), "missing System Overview");
    assert!(md.contains("### Heap Summary"), "missing Heap Summary");
    assert!(
        md.contains("### Class Histogram"),
        "missing Class Histogram"
    );
    assert!(md.contains("## Leak Suspects"), "missing Leak Suspects");
    assert!(md.contains("## Top Consumers"), "missing Top Consumers");
    assert!(
        md.contains("### Biggest Objects"),
        "missing Biggest Objects"
    );
    assert!(
        md.contains("### Biggest Classes"),
        "missing Biggest Classes"
    );
    assert!(
        md.contains("### Biggest Packages"),
        "missing Biggest Packages"
    );
}

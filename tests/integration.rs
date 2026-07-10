#[test]
fn end_to_end_dump0() {
    let path = "/home/i560383/test-heapdumps/dump_0_fj-kmeans.hprof";
    if !std::path::Path::new(path).exists() { return; }
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_hprof-analyzer"))
        .arg(path)
        .output()
        .expect("failed to run hprof-analyzer");
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let md = String::from_utf8_lossy(&out.stdout);
    assert!(md.contains("## System Overview"), "missing System Overview");
    assert!(md.contains("### Heap Summary"), "missing Heap Summary");
    assert!(md.contains("### Class Histogram"), "missing Class Histogram");
    assert!(md.contains("## Leak Suspects"), "missing Leak Suspects");
    assert!(md.contains("## Top Consumers"), "missing Top Consumers");
    assert!(md.contains("### Biggest Objects"), "missing Biggest Objects");
    assert!(md.contains("### Biggest Classes"), "missing Biggest Classes");
    assert!(md.contains("### Biggest Packages"), "missing Biggest Packages");
}

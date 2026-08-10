/// Tests the -XX:OnOutOfMemoryError JVM hook workflow:
/// a JVM process OOMs, writes a heap dump, and hprof-analyzer
/// is invoked automatically via the hook to produce a report.
use std::path::PathBuf;
use std::process::Command;

fn hprof_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_hprof-analyzer"))
}

/// Minimal Java program that immediately triggers an OOM.
/// Uses -Xmx32m so the dump is tiny and fast.
const OOM_CLASS: &str = "OomTrigger";
const OOM_SOURCE: &str = r#"
public class OomTrigger {
    public static void main(String[] args) throws Exception {
        // Allocate until OOM
        java.util.List<byte[]> sink = new java.util.ArrayList<>();
        while (true) {
            sink.add(new byte[1024 * 1024]);
        }
    }
}
"#;

fn compile_oom_class(dir: &std::path::Path) -> std::path::PathBuf {
    let src = dir.join(format!("{OOM_CLASS}.java"));
    std::fs::write(&src, OOM_SOURCE).expect("write OomTrigger.java");
    let status = Command::new("javac")
        .arg(&src)
        .current_dir(dir)
        .status()
        .expect("javac");
    assert!(status.success(), "javac failed");
    dir.to_path_buf()
}

#[test]
fn oom_hook_produces_valid_report() {
    let bin = hprof_bin();
    let dir = tempfile::tempdir().expect("tempdir");
    let d = dir.path();

    compile_oom_class(d);

    let dump = d.join("oom.hprof");
    let report = d.join("oom_report.json");

    // Hook command: run hprof-analyzer on the dump, write JSON report.
    // %p is replaced by the JVM with the PID — but for heap dump path we use
    // a fixed path via -XX:HeapDumpPath instead, since %p in OnOutOfMemoryError
    // refers to the process PID, not the dump file.
    let hook_cmd = format!(
        "{} {} --format json {}",
        bin.display(),
        dump.display(),
        report.display(),
    );

    let output = Command::new("java")
        .args([
            "-Xmx32m",
            "-XX:+HeapDumpOnOutOfMemoryError",
            &format!("-XX:HeapDumpPath={}", dump.display()),
            &format!("-XX:OnOutOfMemoryError={hook_cmd}"),
            "-cp",
            d.to_str().unwrap(),
            OOM_CLASS,
        ])
        .output()
        .expect("java");

    // JVM exits non-zero on OOM — that's expected.
    assert!(
        !output.status.success(),
        "expected JVM to exit non-zero on OOM"
    );

    // Dump must exist.
    assert!(dump.exists(), "heap dump not written by JVM");

    // Report must exist and be valid JSON with schema_version.
    assert!(
        report.exists(),
        "report not produced by OnOutOfMemoryError hook\nstderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&report).unwrap())
        .expect("report is not valid JSON");

    assert!(
        json.get("schema_version").is_some(),
        "report missing schema_version"
    );
    assert_eq!(
        json.get("truncated_input").and_then(|v| v.as_bool()),
        Some(false),
        "report should not be truncated"
    );

    let total_objects = json
        .pointer("/overview/total_objects")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    assert!(
        total_objects > 0,
        "report has no objects (overview.total_objects = 0)"
    );
}

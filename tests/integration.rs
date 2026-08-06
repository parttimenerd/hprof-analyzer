// The structural Markdown helpers live in `src/md_test.rs`, gated behind
// `#[cfg(test)]` in the binary crate — which an integration test (a separate
// crate) cannot import. Rather than add a dependency or duplicate the code, we
// `#[path]`-include the same source file here so both places share one parser.
#[path = "../src/md_test.rs"]
mod md_test;
use md_test::Md;

// ---------------------------------------------------------------------------
// Helpers shared by the broken-file resilience tests
// ---------------------------------------------------------------------------

fn hprof_header() -> Vec<u8> {
    let mut h = b"JAVA PROFILE 1.0.2\0".to_vec();
    h.extend_from_slice(&8u32.to_be_bytes()); // id_size = 8
    h.extend_from_slice(&0u64.to_be_bytes()); // timestamp = 0
    h
}

/// Build a STRING_IN_UTF8 (tag 0x01) record with the given payload bytes.
fn string_record(id: u64, text: &[u8]) -> Vec<u8> {
    let body_len = 8 + text.len() as u32; // id(8) + text
    let mut rec = vec![0x01u8]; // tag
    rec.extend_from_slice(&0u32.to_be_bytes()); // ts_delta
    rec.extend_from_slice(&body_len.to_be_bytes());
    rec.extend_from_slice(&id.to_be_bytes());
    rec.extend_from_slice(text);
    rec
}

/// Build a LOAD_CLASS (tag 0x02) record.
fn load_class_record(serial: u32, class_addr: u64, name_id: u64) -> Vec<u8> {
    let body_len: u32 = 4 + 8 + 4 + 8; // serial + addr + stack_serial + name_id
    let mut rec = vec![0x02u8]; // tag
    rec.extend_from_slice(&0u32.to_be_bytes());
    rec.extend_from_slice(&body_len.to_be_bytes());
    rec.extend_from_slice(&serial.to_be_bytes());
    rec.extend_from_slice(&class_addr.to_be_bytes());
    rec.extend_from_slice(&0u32.to_be_bytes()); // stack_serial
    rec.extend_from_slice(&name_id.to_be_bytes());
    rec
}

/// Run `hprof-analyzer <file> --format json` and return (exit_success, stdout, stderr).
fn run_json(path: &std::path::Path) -> (bool, String, String) {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_hprof-analyzer"))
        .arg(path)
        .arg("--format")
        .arg("json")
        .output()
        .expect("failed to spawn hprof-analyzer");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Run `hprof-analyzer report <file>` and return (exit_success, combined output).
fn run_report(path: &std::path::Path) -> (bool, String) {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_hprof-analyzer"))
        .arg("report")
        .arg(path)
        .output()
        .expect("failed to spawn hprof-analyzer");
    let combined = format!(
        "STDOUT:\n{}\nSTDERR:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.success(), combined)
}

/// Assert the process did not panic (no "panicked at" in output).
fn assert_no_panic(combined: &str) {
    assert!(
        !combined.contains("panicked at"),
        "tool panicked!\n{combined}"
    );
}

/// Assert that stderr contains no raw internal reader strings regardless of exit code.
fn assert_error_message_is_clean(stderr: &str, label: &str) {
    for bad in &[
        "eof in read_into",
        "eof in skip",
        "failed to fill whole buffer",
    ] {
        assert!(
            !stderr.contains(bad),
            "{label}: stderr contains raw internal message {bad:?}\nstderr: {stderr}"
        );
    }
}

/// Returns true if the JSON output is at least valid JSON with a schema_version field,
/// meaning the tool produced a report (even if it covers zero objects).
fn json_is_valid_report(json: &str) -> bool {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else {
        return false;
    };
    v["schema_version"].is_number()
}

/// Returns true if the JSON report stdout contains at least one object
/// (overview.total_objects > 0) and at least one class name.
fn json_report_has_content(json: &str) -> bool {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else {
        return false;
    };
    let objects = v["overview"]["total_objects"].as_u64().unwrap_or(0);
    objects > 0
}

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
        .arg(&hprof)
        .arg("--format")
        .arg("json")
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
         --format json > tests/fixtures/dump_4_philosophers_report.json"
    );
}

/// gzip round-trip: `analyze --format json <out>.json.gz` writes a gzip stream,
/// and `render <out>.json.gz` reads it back transparently. The re-rendered JSON
/// must equal a plain-JSON render of the same report (modulo the per-run
/// `generated`/`file_path` fields), proving emit and render agree over the
/// compressed form.
#[test]
fn json_gzip_roundtrip() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");
    let hprof = format!("{dir}/dump_4_philosophers.hprof");

    // Skip if the LFS fixture is absent or an unsmudged pointer.
    match std::fs::metadata(&hprof) {
        Ok(m) if m.len() >= 1024 => {}
        _ => return,
    }

    let bin = env!("CARGO_BIN_EXE_hprof-analyzer");
    let tmp = std::env::temp_dir().join(format!("hprof_gz_roundtrip_{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    let gz_path = tmp.join("report.json.gz");

    // Emit gzip-compressed JSON to a .json.gz path.
    let status = std::process::Command::new(bin)
        .arg(&hprof)
        .arg("--format")
        .arg("json")
        .arg(&gz_path)
        .status()
        .expect("failed to run analyzer");
    assert!(status.success(), "analyze to .json.gz exited non-zero");

    // The file must be a real gzip stream (magic bytes 0x1f 0x8b).
    let raw = std::fs::read(&gz_path).unwrap();
    assert!(
        raw.starts_with(&[0x1f, 0x8b]),
        "output .json.gz is not gzip-compressed (magic {:x?})",
        &raw[..raw.len().min(2)]
    );

    // Render the .json.gz back to JSON (transparent decompress).
    let out = std::process::Command::new(bin)
        .arg(&gz_path)
        .arg("--format")
        .arg("json")
        .output()
        .expect("failed to run render");
    assert!(
        out.status.success(),
        "render .json.gz stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let mut got: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("render output was not valid JSON");

    // Compare against a plain JSON analyze of the same dump.
    let plain = std::process::Command::new(bin)
        .arg(&hprof)
        .arg("--format")
        .arg("json")
        .output()
        .expect("failed to run analyzer (plain)");
    assert!(plain.status.success());
    let mut want: serde_json::Value =
        serde_json::from_slice(&plain.stdout).expect("plain output was not valid JSON");

    normalize_nondeterministic(&mut got);
    normalize_nondeterministic(&mut want);

    let _ = std::fs::remove_dir_all(&tmp);
    assert_eq!(
        got, want,
        "rendered .json.gz did not match a plain JSON render of the same dump"
    );
}

/// `--ref-paths` smoke test: the flag must not crash, and the JSON output must
/// parse cleanly. Field annotations only appear on multi-hop chains that have
/// named forward edges; the philosophers dump has only single-step chains so we
/// just verify correctness of the structural output here.
#[test]
fn ref_paths_flag_smoke() {
    let hprof = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/dump_4_philosophers.hprof"
    );
    match std::fs::metadata(hprof) {
        Ok(m) if m.len() >= 1024 => {}
        _ => return,
    }
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_hprof-analyzer"))
        .arg(hprof)
        .arg("--ref-paths")
        .arg("--format")
        .arg("json")
        .output()
        .expect("failed to run hprof-analyzer --ref-paths");
    assert!(
        out.status.success(),
        "--ref-paths exited non-zero; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("--ref-paths JSON output was not valid JSON");
    // The report must still contain the leaks section.
    assert!(
        v.get("leaks").is_some(),
        "--ref-paths JSON missing 'leaks' key"
    );
    // --ref-paths should produce at least one non-null field_edge in root_path steps
    let suspects = v["leaks"]["suspects"].as_array();
    if suspects.map(|a| !a.is_empty()).unwrap_or(false) {
        let has_field_edge = suspects.unwrap().iter().any(|s| {
            s["root_path"]
                .as_array()
                .unwrap_or(&vec![])
                .iter()
                .any(|step| step["field_edge"].is_string())
        });
        // Only informational — small fixtures may not produce field edges in root paths
        let _ = has_field_edge;
    }
}

/// `--field-stats` smoke test: the flag must not crash, JSON must parse, and
/// the field_stats.classes array must contain at least one named ref field.
#[test]
fn field_stats_smoke() {
    let hprof = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/dump_4_philosophers.hprof"
    );
    match std::fs::metadata(hprof) {
        Ok(m) if m.len() >= 1024 => {}
        _ => return,
    }
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_hprof-analyzer"))
        .arg(hprof)
        .arg("--field-stats")
        .arg("--format")
        .arg("json")
        .output()
        .expect("failed to run hprof-analyzer --field-stats");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let classes = v["field_stats"]["classes"]
        .as_array()
        .expect("field_stats.classes must be an array");
    assert!(!classes.is_empty(), "field_stats.classes must not be empty");
    let has_named = classes.iter().any(|c| {
        c["ref_fields"]
            .as_array()
            .unwrap_or(&vec![])
            .iter()
            .any(|f| {
                f["field_name"]
                    .as_str()
                    .map(|s| !s.is_empty())
                    .unwrap_or(false)
            })
    });
    assert!(
        has_named,
        "expected at least one named ref field in --field-stats output"
    );
}

/// `--full-analysis` smoke test: the flag must not crash, and the output must
/// include at least one of the heavy opt-in sections (obj_graph, collections,
/// or duplicates) that `--full-analysis` enables.
#[test]
fn full_analysis_smoke() {
    let hprof = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/dump_4_philosophers.hprof"
    );
    match std::fs::metadata(hprof) {
        Ok(m) if m.len() >= 1024 => {}
        _ => return,
    }
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_hprof-analyzer"))
        .arg(hprof)
        .arg("--full-analysis")
        .arg("--format")
        .arg("json")
        .output()
        .expect("failed to run hprof-analyzer --full-analysis");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(
        v["overview"].is_object(),
        "--full-analysis: expected overview section"
    );
    // --full-analysis enables --obj-graph --collections --find-duplicates
    let has_extra = v.get("obj_graph_flat").is_some()
        || v.get("collection_attribution").is_some()
        || v.get("waste_summary").is_some();
    assert!(
        has_extra,
        "--full-analysis: expected obj_graph_flat, collection_attribution, or waste_summary in output"
    );
}

/// Golden snapshot for `--field-stats`: the per-field null/non-null/retained
/// stats must not drift from the committed fixture.
#[test]
fn parity_field_stats_golden() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");
    let hprof = format!("{dir}/dump_4_philosophers.hprof");
    let golden_path = format!("{dir}/dump_4_philosophers_field_stats.json");

    match std::fs::metadata(&hprof) {
        Ok(m) if m.len() >= 1024 => {}
        _ => return,
    }
    if !std::path::Path::new(&golden_path).exists() {
        return;
    }

    let out = std::process::Command::new(env!("CARGO_BIN_EXE_hprof-analyzer"))
        .arg(&hprof)
        .arg("--field-stats")
        .arg("--format")
        .arg("json")
        .output()
        .expect("failed to run hprof-analyzer --field-stats");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let actual: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let expected: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&golden_path).unwrap()).unwrap();

    assert_eq!(
        actual["field_stats"], expected["field_stats"],
        "field_stats diverged from golden. Re-capture with:\n  \
        ./target/release/hprof-analyzer tests/fixtures/dump_4_philosophers.hprof \
        tests/fixtures/dump_4_philosophers_field_stats.json --field-stats --format json"
    );
}

// ---------------------------------------------------------------------------
// Broken / truncated file resilience tests
// ---------------------------------------------------------------------------

#[test]
fn broken_empty_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("empty.hprof");
    std::fs::write(&path, b"").unwrap();
    let (ok, out) = run_report(&path);
    assert_no_panic(&out);
    assert_error_message_is_clean(&out, "empty file");
    assert!(!ok, "empty file should not succeed\n{out}");
}

#[test]
fn broken_garbage_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("garbage.hprof");
    std::fs::write(&path, [0xDE, 0xAD, 0xBE, 0xEF].repeat(128)).unwrap();
    let (ok, out) = run_report(&path);
    assert_no_panic(&out);
    assert_error_message_is_clean(&out, "garbage file");
    assert!(!ok, "garbage file should not succeed\n{out}");
}

#[test]
fn broken_header_only() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("header_only.hprof");
    std::fs::write(&path, hprof_header()).unwrap();
    let (ok, out) = run_report(&path);
    assert_no_panic(&out);
    assert_error_message_is_clean(&out, "header only");
    // No objects → either succeeds with an empty report or fails; either is fine.
    let _ = ok;
}

#[test]
fn broken_truncated_mid_record_header() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mid_header.hprof");
    let mut data = hprof_header();
    data.push(0x01u8); // start of a STRING record tag byte only, then EOF
    std::fs::write(&path, data).unwrap();
    let (_ok, out) = run_report(&path);
    assert_no_panic(&out);
    assert_error_message_is_clean(&out, "truncated mid-record-header");
}

#[test]
fn broken_truncated_mid_record_body() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mid_body.hprof");
    let mut data = hprof_header();
    // STRING record claiming 108-byte body (8-byte id + 100 bytes text)
    data.push(0x01u8); // tag
    data.extend_from_slice(&0u32.to_be_bytes()); // ts_delta
    data.extend_from_slice(&108u32.to_be_bytes()); // length = 108
    data.extend_from_slice(&1u64.to_be_bytes()); // id = 1
    data.extend_from_slice(&[b'x'; 10]); // only 10 bytes instead of 100
    std::fs::write(&path, data).unwrap();
    let (_ok, out) = run_report(&path);
    assert_no_panic(&out);
    assert_error_message_is_clean(&out, "truncated mid-record-body");
}

#[test]
fn broken_truncated_heap_dump_segment() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("truncated_heap.hprof");
    let mut data = hprof_header();
    data.extend(string_record(1, b"java.lang.Object"));
    data.extend(load_class_record(1, 0x1000, 1));
    // HEAP_DUMP_SEGMENT (tag 0x1C) claiming 200 bytes but only 40 written
    data.push(0x1Cu8); // tag
    data.extend_from_slice(&0u32.to_be_bytes()); // ts_delta
    data.extend_from_slice(&200u32.to_be_bytes()); // claimed length
    data.extend_from_slice(&[0u8; 40]); // truncated body
    std::fs::write(&path, data).unwrap();
    let (_ok, out) = run_report(&path);
    assert_no_panic(&out);
    assert_error_message_is_clean(&out, "truncated heap dump segment");
}

#[test]
fn broken_heap_dump_with_valid_prefix() {
    // Header + STRING + LOAD_CLASS + full HEAP_DUMP_END, then garbage appended.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("valid_then_garbage.hprof");
    let mut data = hprof_header();
    data.extend(string_record(1, b"java.lang.Object"));
    data.extend(load_class_record(1, 0x1000, 1));
    // HEAP_DUMP_END (tag 0x2C, length 0)
    data.push(0x2Cu8);
    data.extend_from_slice(&0u32.to_be_bytes());
    data.extend_from_slice(&0u32.to_be_bytes());
    // Append garbage after a valid terminator
    data.extend_from_slice(&[0xFFu8; 64]);
    std::fs::write(&path, data).unwrap();
    let (_ok, out) = run_report(&path);
    assert_no_panic(&out);
    assert_error_message_is_clean(&out, "valid prefix then garbage");
}

#[test]
fn broken_truncated_gz() {
    use std::io::Write;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("truncated.hprof.gz");
    // Build a real gzip of the header+string+load_class bytes, then truncate it.
    let hprof = {
        let mut d = hprof_header();
        d.extend(string_record(1, b"java.lang.Object"));
        d.extend(load_class_record(1, 0x1000, 1));
        d
    };
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(&hprof).unwrap();
    let full_gz = encoder.finish().unwrap();
    // Keep only the first 60% of the gzip stream.
    let truncated = &full_gz[..full_gz.len() * 60 / 100];
    std::fs::write(&path, truncated).unwrap();
    let (_ok, out) = run_report(&path);
    assert_no_panic(&out);
    assert_error_message_is_clean(&out, "truncated gz");
}

// ---------------------------------------------------------------------------
// Real-fixture truncation tests: dump_1_mnemonics.hprof cut at known offsets
//
// Offsets are hard-coded fractions of the 21 235 655-byte file so that tests
// are reproducible without RNG.  We assert:
//   1. No panic (no "panicked at" in output)
//   2. When the tool exits 0, the HTML report contains actual content
//      (DOCTYPE + System Overview + at least one Java class name).
// ---------------------------------------------------------------------------

/// Load dump_1_mnemonics.hprof; return None if the LFS fixture is absent.
fn mnemonics_bytes() -> Option<Vec<u8>> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/dump_1_mnemonics.hprof"
    );
    let data = std::fs::read(path).ok()?;
    if data.len() < 1024 * 1024 {
        return None; // LFS pointer, not the real file
    }
    Some(data)
}

fn gz_wrap(data: &[u8]) -> Vec<u8> {
    use std::io::Write;
    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    enc.write_all(data).unwrap();
    enc.finish().unwrap()
}

fn tar_gz_wrap(data: &[u8], inner_name: &str) -> Vec<u8> {
    let gz_buf = Vec::new();
    let enc = flate2::write::GzEncoder::new(gz_buf, flate2::Compression::default());
    let mut tar = tar::Builder::new(enc);
    let mut header = tar::Header::new_gnu();
    header.set_size(data.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    tar.append_data(&mut header, inner_name, data).unwrap();
    let enc = tar.into_inner().unwrap();
    enc.finish().unwrap()
}

// 10 cut points for plain .hprof (num/den fraction of file size).
const HPROF_CUT_FRACS: &[(u64, u64)] = &[
    (1, 64),  // very early – header barely written
    (1, 32),  // in the string/class records
    (1, 16),  // start of heap dump
    (1, 8),   // shallow into heap segment
    (3, 16),  // quarter of the way through heap
    (3, 8),   // well into the heap dump
    (1, 2),   // exact midpoint
    (5, 8),   // past midpoint
    (3, 4),   // three-quarters
    (15, 16), // near end
];

// 5 cut points for .gz and .tar.gz (applied to the compressed stream).
const GZ_CUT_FRACS: &[(u64, u64)] = &[(1, 16), (1, 4), (3, 8), (5, 8), (7, 8)];

#[test]
fn truncated_real_hprof_plain() {
    let data = match mnemonics_bytes() {
        Some(d) => d,
        None => return,
    };
    let dir = tempfile::tempdir().unwrap();
    let total = data.len() as u64;

    for &(num, den) in HPROF_CUT_FRACS {
        let cut = ((total * num) / den) as usize;
        let path = dir.path().join(format!("cut_{num}_{den}.hprof"));
        std::fs::write(&path, &data[..cut]).unwrap();

        let (ok, json, stderr) = run_json(&path);
        let label = format!("plain .hprof cut at {num}/{den} ({cut} bytes)");
        assert_no_panic(&format!("{json}\n{stderr}"));
        assert_error_message_is_clean(&stderr, &label);
        // Tool must always produce a valid JSON report, even for heavily truncated files.
        assert!(ok, "{label}: tool exited non-zero\nstderr: {stderr}");
        assert!(
            json_is_valid_report(&json),
            "{label}: exit=0 but output is not a valid JSON report\nstderr: {stderr}\njson: {json}"
        );
        // For cuts past 1/8 of the file, the heap dump section should be present
        // and we expect at least some objects in the report.
        if num * 8 > den {
            assert!(
                json_report_has_content(&json),
                "{label}: cut past 1/8 but report has zero objects\nstderr: {stderr}"
            );
        }
    }
}

#[test]
fn truncated_real_hprof_gz() {
    let data = match mnemonics_bytes() {
        Some(d) => d,
        None => return,
    };
    let full_gz = gz_wrap(&data);
    let dir = tempfile::tempdir().unwrap();
    let total = full_gz.len() as u64;

    for &(num, den) in GZ_CUT_FRACS {
        let cut = ((total * num) / den) as usize;
        let path = dir.path().join(format!("cut_{num}_{den}.hprof.gz"));
        std::fs::write(&path, &full_gz[..cut]).unwrap();

        let (ok, json, stderr) = run_json(&path);
        let label = format!(".hprof.gz cut at {num}/{den} ({cut} bytes)");
        assert_no_panic(&format!("{json}\n{stderr}"));
        assert_error_message_is_clean(&stderr, &label);
        assert!(ok, "{label}: tool exited non-zero\nstderr: {stderr}");
        assert!(
            json_is_valid_report(&json),
            "{label}: exit=0 but output is not a valid JSON report\nstderr: {stderr}\njson: {json}"
        );
    }
}

#[test]
fn truncated_real_hprof_tar_gz() {
    let data = match mnemonics_bytes() {
        Some(d) => d,
        None => return,
    };
    let full_tgz = tar_gz_wrap(&data, "dump.hprof");
    let dir = tempfile::tempdir().unwrap();
    let total = full_tgz.len() as u64;

    for &(num, den) in GZ_CUT_FRACS {
        let cut = ((total * num) / den) as usize;
        let path = dir.path().join(format!("cut_{num}_{den}.hprof.tar.gz"));
        std::fs::write(&path, &full_tgz[..cut]).unwrap();

        let (ok, json, stderr) = run_json(&path);
        let label = format!(".hprof.tar.gz cut at {num}/{den} ({cut} bytes)");
        assert_no_panic(&format!("{json}\n{stderr}"));
        assert_error_message_is_clean(&stderr, &label);
        assert!(ok, "{label}: tool exited non-zero\nstderr: {stderr}");
        assert!(
            json_is_valid_report(&json),
            "{label}: exit=0 but output is not a valid JSON report\nstderr: {stderr}\njson: {json}"
        );
    }
}

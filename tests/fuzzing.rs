use proptest::prelude::*;
use std::io::Write;

// ---------------------------------------------------------------------------
// Helpers (mirrors of integration.rs — cannot import across test binaries)
// ---------------------------------------------------------------------------

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

fn json_is_valid_report(json: &str) -> bool {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else {
        return false;
    };
    v["schema_version"].is_number()
}

fn assert_no_panic(combined: &str) {
    assert!(
        !combined.contains("panicked at"),
        "tool panicked!\n{combined}"
    );
}

fn write_temp(bytes: &[u8], ext: &str) -> tempfile::NamedTempFile {
    let mut f = tempfile::Builder::new()
        .suffix(ext)
        .tempfile()
        .expect("tempfile");
    f.write_all(bytes).expect("write");
    f
}

// ---------------------------------------------------------------------------
// HPROF binary builders
// ---------------------------------------------------------------------------

/// Write an id (either 4 or 8 bytes, big-endian).
fn write_id(buf: &mut Vec<u8>, id: u64, id_size: u32) {
    if id_size == 4 {
        buf.extend_from_slice(&(id as u32).to_be_bytes());
    } else {
        buf.extend_from_slice(&id.to_be_bytes());
    }
}

/// `"JAVA PROFILE 1.0.2\0" + u32(id_size) + u64(timestamp=0)`
fn hprof_header(id_size: u32) -> Vec<u8> {
    let mut h = b"JAVA PROFILE 1.0.2\0".to_vec();
    h.extend_from_slice(&id_size.to_be_bytes());
    h.extend_from_slice(&0u64.to_be_bytes());
    h
}

/// Wrap a body in a top-level record: `tag(1) + ts_delta(4) + body_len(4) + body`.
fn make_record(tag: u8, body: &[u8]) -> Vec<u8> {
    let mut r = vec![tag];
    r.extend_from_slice(&0u32.to_be_bytes()); // ts_delta
    r.extend_from_slice(&(body.len() as u32).to_be_bytes());
    r.extend_from_slice(body);
    r
}

/// STRING_IN_UTF8 (0x01): id + text bytes.
fn make_string(id: u64, text: &[u8], id_size: u32) -> Vec<u8> {
    let mut body = Vec::new();
    write_id(&mut body, id, id_size);
    body.extend_from_slice(text);
    make_record(0x01, &body)
}

/// LOAD_CLASS (0x02): serial(4) + class_id + stack_serial(4) + name_id.
fn make_load_class(serial: u32, class_id: u64, name_id: u64, id_size: u32) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&serial.to_be_bytes());
    write_id(&mut body, class_id, id_size);
    body.extend_from_slice(&0u32.to_be_bytes()); // stack_serial
    write_id(&mut body, name_id, id_size);
    make_record(0x02, &body)
}

/// Minimal CLASS_DUMP sub-record (inside a heap dump body).
/// `class_id + serial(4) + super_id + loader(id) + signers(id) + domain(id) +
///  res1(id) + res2(id) + inst_size(4) + cp_count(2)=0 + static_count(2)=0 +
///  field_count(2) + [N * (name_id(id) + type(1))]`
fn make_class_dump(class_id: u64, super_id: u64, fields: &[(u64, u8)], id_size: u32) -> Vec<u8> {
    let mut body = Vec::new();
    write_id(&mut body, class_id, id_size);
    body.extend_from_slice(&1u32.to_be_bytes()); // serial
    write_id(&mut body, super_id, id_size); // super class id (0 = none)
    write_id(&mut body, 0u64, id_size); // loader
    write_id(&mut body, 0u64, id_size); // signers
    write_id(&mut body, 0u64, id_size); // domain
    write_id(&mut body, 0u64, id_size); // res1
    write_id(&mut body, 0u64, id_size); // res2
    // instance_size: just use 0 (parser doesn't enforce field-sum match)
    body.extend_from_slice(&0u32.to_be_bytes());
    body.extend_from_slice(&0u16.to_be_bytes()); // cp_count
    body.extend_from_slice(&0u16.to_be_bytes()); // static_count
    body.extend_from_slice(&(fields.len() as u16).to_be_bytes());
    for (name_id, type_code) in fields {
        write_id(&mut body, *name_id, id_size);
        body.push(*type_code);
    }
    let mut rec = vec![0x20u8]; // CLASS_DUMP sub-tag
    rec.extend_from_slice(&body);
    rec
}

/// INSTANCE_DUMP (0x21): obj_id + serial(4) + class_id + data_len(4) + data.
fn make_instance_dump(obj_id: u64, class_id: u64, data: &[u8], id_size: u32) -> Vec<u8> {
    let mut body = Vec::new();
    write_id(&mut body, obj_id, id_size);
    body.extend_from_slice(&0u32.to_be_bytes()); // serial
    write_id(&mut body, class_id, id_size);
    body.extend_from_slice(&(data.len() as u32).to_be_bytes());
    body.extend_from_slice(data);
    let mut rec = vec![0x21u8];
    rec.extend_from_slice(&body);
    rec
}

/// PRIM_ARRAY_DUMP (0x23): obj_id + serial(4) + count(4) + type_code(1) + elems.
fn make_prim_array(obj_id: u64, type_code: u8, elems: &[u8], id_size: u32) -> Vec<u8> {
    let elem_size: usize = match type_code {
        2 => 4,  // object (but this is a prim array, use int)
        4 => 1,  // boolean
        5 => 2,  // char
        6 => 4,  // float
        7 => 8,  // double
        8 => 1,  // byte
        9 => 2,  // short
        10 => 4, // int
        11 => 8, // long
        _ => 1,
    };
    let count = elems.len().checked_div(elem_size).unwrap_or(0);
    let actual_elems = &elems[..count * elem_size];
    let mut body = Vec::new();
    write_id(&mut body, obj_id, id_size);
    body.extend_from_slice(&0u32.to_be_bytes()); // serial
    body.extend_from_slice(&(count as u32).to_be_bytes());
    body.push(type_code);
    body.extend_from_slice(actual_elems);
    let mut rec = vec![0x23u8];
    rec.extend_from_slice(&body);
    rec
}

/// OBJ_ARRAY_DUMP (0x22): obj_id + serial(4) + count(4) + elem_class_id + [count * id].
fn make_obj_array(obj_id: u64, elem_class_id: u64, elem_ids: &[u64], id_size: u32) -> Vec<u8> {
    let mut body = Vec::new();
    write_id(&mut body, obj_id, id_size);
    body.extend_from_slice(&0u32.to_be_bytes()); // serial
    body.extend_from_slice(&(elem_ids.len() as u32).to_be_bytes());
    write_id(&mut body, elem_class_id, id_size);
    for &id in elem_ids {
        write_id(&mut body, id, id_size);
    }
    let mut rec = vec![0x22u8];
    rec.extend_from_slice(&body);
    rec
}

/// ROOT_UNKNOWN (0xFF): just an id.
fn make_root_unknown(id: u64, id_size: u32) -> Vec<u8> {
    let mut rec = vec![0xFFu8];
    write_id(&mut rec, id, id_size);
    rec
}

/// Wrap sub-records in a HEAP_DUMP (0x0C) top-level record + HEAP_DUMP_END (0x2C).
fn make_heap_dump(sub_records: Vec<u8>) -> Vec<u8> {
    let mut out = make_record(0x0C, &sub_records);
    out.extend_from_slice(&make_record(0x2C, &[]));
    out
}

/// Gzip-compress bytes using flate2.
fn wrap_gzip(bytes: &[u8]) -> Vec<u8> {
    use flate2::Compression;
    use flate2::write::GzEncoder;
    let mut enc = GzEncoder::new(Vec::new(), Compression::fast());
    enc.write_all(bytes).unwrap();
    enc.finish().unwrap()
}

/// Wrap bytes as a single "dump.hprof" entry in a tar.gz archive.
fn wrap_tar_gz(bytes: &[u8]) -> Vec<u8> {
    use flate2::Compression;
    use flate2::write::GzEncoder;
    let gz_buf = Vec::new();
    let enc = GzEncoder::new(gz_buf, Compression::fast());
    let mut ar = tar::Builder::new(enc);
    let mut header = tar::Header::new_gnu();
    header.set_size(bytes.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    ar.append_data(&mut header, "dump.hprof", bytes).unwrap();
    let enc = ar.into_inner().unwrap();
    enc.finish().unwrap()
}

// ---------------------------------------------------------------------------
// Strategy
// ---------------------------------------------------------------------------

prop_compose! {
    fn synthetic_hprof()(
        use_id8 in prop::bool::weighted(0.2),
        strings in prop::collection::vec("[a-zA-Z][a-zA-Z0-9_$]*".prop_map(|s| s.into_bytes()), 0..=8usize),
        num_classes in 1usize..=4,
        instances_data in prop::collection::vec(
            prop::collection::vec(any::<u8>(), 0..=64usize),
            0..=8usize,
        ),
        num_prim_arrays in 0usize..=4,
        prim_elems in prop::collection::vec(
            prop::collection::vec(any::<u8>(), 0..=64usize),
            0..=4usize,
        ),
        num_obj_arrays in 0usize..=4,
        obj_array_counts in prop::collection::vec(0usize..=8, 0..=4usize),
        num_roots in 0usize..=8,
    ) -> Vec<u8> {
        let id_size: u32 = if use_id8 { 8 } else { 4 };

        let mut out = hprof_header(id_size);

        // ID allocator
        let mut next_id: u64 = 0x100;
        let mut alloc = || { let id = next_id; next_id += 1; id };

        // String records
        let mut string_ids: Vec<u64> = Vec::new();
        for text in &strings {
            let id = alloc();
            string_ids.push(id);
            out.extend_from_slice(&make_string(id, text, id_size));
        }

        // Class records: each class gets a name string id + LOAD_CLASS record
        let mut class_ids: Vec<u64> = Vec::new();
        for i in 0..num_classes {
            let class_id = alloc();
            class_ids.push(class_id);
            let name_id = if i < string_ids.len() { string_ids[i] } else { alloc() };
            out.extend_from_slice(&make_load_class((i + 1) as u32, class_id, name_id, id_size));
        }

        // Build heap dump body
        let mut heap = Vec::new();

        // Class dumps
        for &class_id in &class_ids {
            heap.extend_from_slice(&make_class_dump(class_id, 0, &[], id_size));
        }

        // Instances
        for data in &instances_data {
            let obj_id = alloc();
            let class_id = class_ids[obj_id as usize % class_ids.len()];
            heap.extend_from_slice(&make_instance_dump(obj_id, class_id, data, id_size));
        }

        // Primitive arrays  (type codes: byte=8, int=10, long=11)
        let type_codes = [8u8, 10, 11];
        for i in 0..num_prim_arrays {
            let obj_id = alloc();
            let tc = type_codes[i % type_codes.len()];
            let elems = prim_elems.get(i).map(|v| v.as_slice()).unwrap_or(&[]);
            heap.extend_from_slice(&make_prim_array(obj_id, tc, elems, id_size));
        }

        // Object arrays
        for i in 0..num_obj_arrays {
            let obj_id = alloc();
            let elem_class = class_ids[i % class_ids.len()];
            let count = obj_array_counts.get(i).copied().unwrap_or(0).min(8);
            let elem_ids: Vec<u64> = (0..count).map(|_| alloc()).collect();
            heap.extend_from_slice(&make_obj_array(obj_id, elem_class, &elem_ids, id_size));
        }

        // Roots
        for _ in 0..num_roots {
            let root_id = alloc();
            heap.extend_from_slice(&make_root_unknown(root_id, id_size));
        }

        out.extend_from_slice(&make_heap_dump(heap));
        out
    }
}

// ---------------------------------------------------------------------------
// Test 1: valid synthetic → valid report
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig { cases: 300, max_shrink_iters: 50, ..Default::default() })]
    #[test]
    fn prop_valid_synthetic_hprof(bytes in synthetic_hprof()) {
        let f = write_temp(&bytes, ".hprof");
        let (ok, stdout, stderr) = run_json(f.path());
        assert_no_panic(&(stdout.clone() + &stderr));
        prop_assert!(ok, "exit non-zero\nstderr: {stderr}");
        prop_assert!(json_is_valid_report(&stdout), "bad JSON\nstdout: {:.200}", stdout);
    }
}

// ---------------------------------------------------------------------------
// Test 2: truncated synthetic → exit 0 + valid JSON
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig { cases: 1000, max_shrink_iters: 30, ..Default::default() })]
    #[test]
    fn prop_truncated_synthetic(
        bytes in synthetic_hprof(),
        cut_frac in 0.0f64..=1.0f64,
    ) {
        let cut = ((bytes.len() as f64) * cut_frac) as usize;
        let f = write_temp(&bytes[..cut], ".hprof");
        let (ok, stdout, stderr) = run_json(f.path());
        assert_no_panic(&(stdout.clone() + &stderr));
        prop_assert!(ok, "truncated exit non-zero at cut {cut}/{}\nstderr: {stderr}", bytes.len());
        prop_assert!(json_is_valid_report(&stdout), "truncated bad JSON\nstdout: {:.200}", stdout);
    }
}

// ---------------------------------------------------------------------------
// Test 3: truncated synthetic gzip → exit 0 + valid JSON
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig { cases: 300, max_shrink_iters: 30, ..Default::default() })]
    #[test]
    fn prop_truncated_synthetic_gz(
        bytes in synthetic_hprof(),
        cut_frac in 0.0f64..=1.0f64,
    ) {
        let gz = wrap_gzip(&bytes);
        let cut = ((gz.len() as f64) * cut_frac) as usize;
        let f = write_temp(&gz[..cut], ".hprof.gz");
        let (ok, stdout, stderr) = run_json(f.path());
        assert_no_panic(&(stdout.clone() + &stderr));
        prop_assert!(ok, "gz truncated exit non-zero\nstderr: {stderr}");
        prop_assert!(json_is_valid_report(&stdout), "gz truncated bad JSON\nstdout: {:.200}", stdout);
    }
}

// ---------------------------------------------------------------------------
// Test 4: truncated synthetic tar.gz → exit 0 + valid JSON
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig { cases: 200, max_shrink_iters: 20, ..Default::default() })]
    #[test]
    fn prop_truncated_synthetic_tar_gz(
        bytes in synthetic_hprof(),
        cut_frac in 0.0f64..=1.0f64,
    ) {
        let tgz = wrap_tar_gz(&bytes);
        let cut = ((tgz.len() as f64) * cut_frac) as usize;
        let f = write_temp(&tgz[..cut], ".hprof.tar.gz");
        let (ok, stdout, stderr) = run_json(f.path());
        assert_no_panic(&(stdout.clone() + &stderr));
        prop_assert!(ok, "tar.gz truncated exit non-zero\nstderr: {stderr}");
        prop_assert!(json_is_valid_report(&stdout), "tar.gz truncated bad JSON\nstdout: {:.200}", stdout);
    }
}

// ---------------------------------------------------------------------------
// Test 5: pure random bytes → no panic; if exit 0, valid JSON; no internal strings
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig { cases: 2000, max_shrink_iters: 10, ..Default::default() })]
    #[test]
    fn prop_random_bytes(
        bytes in prop::collection::vec(any::<u8>(), 0..=4096usize),
    ) {
        let f = write_temp(&bytes, ".hprof");
        let (ok, stdout, stderr) = run_json(f.path());
        assert_no_panic(&(stdout.clone() + &stderr));
        if ok {
            prop_assert!(json_is_valid_report(&stdout), "exit 0 but bad JSON\nstdout: {:.200}", stdout);
        }
        for bad in &["eof in read_into", "eof in skip", "failed to fill whole buffer"] {
            prop_assert!(!stderr.contains(bad), "raw internal error {bad:?} in stderr:\n{stderr}");
        }
    }
}

// ---------------------------------------------------------------------------
// Test 6: random bytes in gz / tar.gz containers → no panic
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig { cases: 500, max_shrink_iters: 10, ..Default::default() })]
    #[test]
    fn prop_random_bytes_gz(
        bytes in prop::collection::vec(any::<u8>(), 0..=4096usize),
    ) {
        for ext in &[".hprof.gz", ".hprof.tar.gz"] {
            let f = write_temp(&bytes, ext);
            let (ok, stdout, stderr) = run_json(f.path());
            assert_no_panic(&(stdout.clone() + &stderr));
            if ok {
                prop_assert!(
                    json_is_valid_report(&stdout),
                    "exit 0 but bad JSON for {ext}\nstdout: {:.200}", stdout
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Test 7: larger random byte buffers (up to 256 KB) → no panic
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig { cases: 200, max_shrink_iters: 10, ..Default::default() })]
    #[test]
    fn prop_random_bytes_large(
        bytes in prop::collection::vec(any::<u8>(), 4096..=262144usize),
    ) {
        let f = write_temp(&bytes, ".hprof");
        let (ok, stdout, stderr) = run_json(f.path());
        assert_no_panic(&(stdout.clone() + &stderr));
        if ok {
            prop_assert!(json_is_valid_report(&stdout), "exit 0 but bad JSON\nstdout: {:.200}", stdout);
        }
        for bad in &["eof in read_into", "eof in skip", "failed to fill whole buffer"] {
            prop_assert!(!stderr.contains(bad), "raw internal error {bad:?} in stderr:\n{stderr}");
        }
    }
}

// ---------------------------------------------------------------------------
// Test 8: larger random bytes in gz / tar.gz (up to 256 KB)
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig { cases: 100, max_shrink_iters: 10, ..Default::default() })]
    #[test]
    fn prop_random_bytes_large_gz(
        bytes in prop::collection::vec(any::<u8>(), 4096..=262144usize),
    ) {
        for ext in &[".hprof.gz", ".hprof.tar.gz"] {
            let f = write_temp(&bytes, ext);
            let (ok, stdout, stderr) = run_json(f.path());
            assert_no_panic(&(stdout.clone() + &stderr));
            if ok {
                prop_assert!(
                    json_is_valid_report(&stdout),
                    "exit 0 but bad JSON for {ext}\nstdout: {:.200}", stdout
                );
            }
        }
    }
}

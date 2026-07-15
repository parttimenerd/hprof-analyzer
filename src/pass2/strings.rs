//! Pass-2 String analysis: duplicate-String census, length stats,
//! String-holder ranking, and the shared Java-String decoder.

use std::collections::HashMap;

use crate::{
    pass1::Pass1,
    reader::HprofReader,
    types::{HprofType, heap, tags},
};

use std::io::{self, ErrorKind};

use super::{
    CharArrayWaste, CharArrayWasteRow, DupStringSample, DupStrings, StrLenBucket, StrLenStats,
    StringHolder, build_field_plans, field_offset, read_ref, scan_prim_arrays, skip_class_dump,
};

/// Max retained sample text length (bytes) for a most-duplicated String — bounds
/// RSS regardless of how long the dump's Strings are.
pub(crate) const MAX_STR_SAMPLE: usize = 200;
/// Top-N cutoff for both most-duplicated strings and String-holding classes.
pub(crate) const TOP_STRINGS_N: usize = 25;
/// Top-N cutoff for the longest DISTINCT String values (view #5 find_strings).
pub(crate) const TOP_STRINGS_BY_LEN: usize = 25;
/// Top-N cutoff for the most-wasteful backing arrays (view #15 char[] waste).
pub(crate) const CHAR_ARRAY_WASTE_TOP: usize = 25;

/// Full-file sequential scan invoking `f(obj_addr, class_id, blob)` for EVERY
/// INSTANCE_DUMP record, materializing each instance's blob into a reused
/// scratch buffer. Unlike [`scan_instance_blobs`], this does NOT filter by an
/// address set — the caller decides per-class (cheaply, via a memoized
/// predicate) which instances are of interest. This lets a caller enumerate
/// ALL instances of a class (e.g. every `java.lang.String`) without ever
/// building an all-addresses HashSet, which would blow up RSS on large dumps.
///
/// The skip skeleton for every non-INSTANCE_DUMP sub-record is identical to
/// `scan_instance_blobs`; only the INSTANCE_DUMP arm differs (it always reads
/// the blob and calls `f`).
pub(crate) fn scan_all_instances<F: FnMut(u64, u64, &[u8])>(
    path: &str,
    id_size: u8,
    mut f: F,
) -> io::Result<()> {
    let ids = id_size as u64;
    let mut r = HprofReader::open(path)?;
    let mut scratch: Vec<u8> = Vec::with_capacity(256);
    loop {
        let tag = match r.u1() {
            Err(e) if e.kind() == ErrorKind::UnexpectedEof => break,
            other => other?,
        };
        let _ts = r.u4()?;
        let length = r.u4()? as u64;
        match tag {
            tags::HEAP_DUMP | tags::HEAP_DUMP_SEGMENT => {
                let mut remaining = length;
                while remaining > 0 {
                    let sub_tag = r.u1()?;
                    remaining -= 1;
                    match sub_tag {
                        heap::ROOT_UNKNOWN | heap::ROOT_MONITOR_USED | heap::ROOT_STICKY_CLASS => {
                            r.skip(ids)?;
                            remaining -= ids;
                        }
                        heap::ROOT_JNI_GLOBAL => {
                            r.skip(2 * ids)?;
                            remaining -= 2 * ids;
                        }
                        heap::ROOT_JNI_LOCAL | heap::ROOT_JAVA_FRAME | heap::ROOT_THREAD_OBJ => {
                            r.skip(ids + 8)?;
                            remaining -= ids + 8;
                        }
                        heap::ROOT_NATIVE_STACK | heap::ROOT_THREAD_BLOCK => {
                            r.skip(ids + 4)?;
                            remaining -= ids + 4;
                        }
                        heap::HEAP_DUMP_INFO => {
                            r.skip(4 + ids)?;
                            remaining -= 4 + ids;
                        }
                        heap::CLASS_DUMP => {
                            let consumed = skip_class_dump(&mut r, id_size)?;
                            remaining -= consumed;
                        }
                        heap::INSTANCE_DUMP => {
                            let addr = r.id()?;
                            r.skip(4)?;
                            let class_id = r.id()?;
                            let data_len = r.u4()? as u64;
                            remaining -= ids + 4 + ids + 4 + data_len;
                            r.read_bytes_reuse(&mut scratch, data_len as usize)?;
                            f(addr, class_id, &scratch);
                        }
                        heap::OBJ_ARRAY_DUMP => {
                            r.skip(ids + 4)?;
                            let count = r.u4()? as u64;
                            r.skip(ids)?;
                            let byte_len = count.saturating_mul(ids);
                            r.skip(byte_len)?;
                            remaining -= ids + 4 + 4 + ids + byte_len;
                        }
                        heap::PRIM_ARRAY_DUMP => {
                            r.skip(ids + 4)?;
                            let count = r.u4()? as u64;
                            let elem_type = r.u1()?;
                            let esz = HprofType::from_code(elem_type)
                                .map(|t| t.byte_size() as u64)
                                .unwrap_or(1);
                            r.skip(count * esz)?;
                            remaining -= ids + 4 + 4 + 1 + count * esz;
                        }
                        other => {
                            return Err(io::Error::new(
                                ErrorKind::InvalidData,
                                format!("unknown heap sub-tag 0x{other:02x} in dup-string scan"),
                            ));
                        }
                    }
                }
            }
            tags::HEAP_DUMP_END => break,
            _ => r.skip(length)?,
        }
    }
    Ok(())
}

/// Compute an approximate duplicate-`java.lang.String` report. Opt-in
/// (`--dup-strings`); adds up to four extra full-file scans. RSS stays bounded
/// because decoded String bytes are NEVER retained wholesale — each value is
/// hashed to 64 bits and only `(hash -> (count, len))` is kept; exact text is
/// recovered for only the ≤N winners (each capped at `MAX_STR_SAMPLE` bytes).
///
/// Flow:
///   Pass A (`scan_all_instances`): for EVERY INSTANCE_DUMP, a memoized
///     per-class predicate decides whether the class is a `java.lang.String`
///     class (via `field_offset(.,"value","java/lang/String",.)==Object`). For
///     each String instance, read its backing-array ref + coder from the blob
///     and record one `(arr_addr, coder)` entry per instance, its `arr_addr ->
///     coder` (first-seen), and its own instance address into `string_addrs`.
///   Pass B (`scan_prim_arrays`, ONE call over the wanted array-addr set):
///     decode each DISTINCT array once, hash the decoded value, and store
///     `arr_addr -> (value_hash, len)`. Bytes are dropped immediately.
///   Fold: for each String INSTANCE (Pass-A list), look up its array's
///     `(value_hash, len)` and bump `value_hash -> (count, len)`; also derive
///     the length histogram/stats. The dedup unit is the String instance, while
///     each array is decoded only once.
///   Pass C (`scan_prim_arrays` over ≤N winners): recover exact truncated text
///     for the most-duplicated values only.
///   Pass D (`compute_string_holders`, `scan_all_instances`): credit each
///     owning class for every object-field reference to a String instance.
pub(crate) fn resolve_duplicate_strings(path: &str, p1: &Pass1) -> io::Result<DupStrings> {
    use std::collections::HashSet;
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let id_size = p1.id_size;
    let obj_ref_width = id_size as usize;
    let class_map = &p1.class_map;
    let strings = &p1.strings;

    // ── Pass A: enumerate every String instance → (arr_addr, coder) ──────────
    // Memoize the value/coder offsets per class id; None means "not a String
    // class" (or its `value` field is missing / not an Object). Also collect the
    // set of String INSTANCE addresses (for the strings-held-by-class walk).
    let mut str_off_cache: HashMap<u64, Option<(usize, Option<usize>)>> = HashMap::new();
    // One entry per String INSTANCE (duplicates of the same arr_addr allowed).
    let mut per_instance: Vec<(u64, u8)> = Vec::new();
    // arr_addr → first-seen coder; also seeds the wanted set for Pass B.
    let mut arr_coder: HashMap<u64, u8> = HashMap::new();
    // Every java.lang.String instance address — bounded by #Strings * 8 bytes.
    let mut string_addrs: HashSet<u64> = HashSet::new();
    let mut total_string_instances: u64 = 0;

    scan_all_instances(path, id_size, |obj_addr, class_id, blob| {
        let offs = *str_off_cache.entry(class_id).or_insert_with(|| {
            let value_off = match field_offset(
                class_id,
                "value",
                "java/lang/String",
                class_map,
                strings,
                obj_ref_width,
            ) {
                Some((off, HprofType::Object)) => off as usize,
                _ => return None,
            };
            let coder_off = match field_offset(
                class_id,
                "coder",
                "java/lang/String",
                class_map,
                strings,
                obj_ref_width,
            ) {
                Some((off, HprofType::Byte)) => Some(off as usize),
                _ => None,
            };
            Some((value_off, coder_off))
        });
        let Some((value_off, coder_off)) = offs else {
            return;
        };
        if value_off + obj_ref_width > blob.len() {
            return;
        }
        let arr_ref = read_ref(&blob[value_off..], obj_ref_width);
        if arr_ref == 0 {
            return;
        }
        // Java 8 char[]: no coder field → UTF16 (coder 1).
        let coder = match coder_off {
            Some(co) if co < blob.len() => blob[co],
            _ => 1,
        };
        total_string_instances += 1;
        per_instance.push((arr_ref, coder));
        arr_coder.entry(arr_ref).or_insert(coder);
        string_addrs.insert(obj_addr);
    })?;

    if per_instance.is_empty() {
        return Ok(DupStrings::default());
    }

    // ── Pass B: decode each distinct backing array once → (hash, len) ────────
    // Bytes are decoded, hashed, then DROPPED — only the 64-bit hash + length
    // survive, so this is bounded by #distinct arrays regardless of dump size.
    let wanted_arrays: HashSet<u64> = arr_coder.keys().copied().collect();
    let mut arr_hash: HashMap<u64, (u64, u32)> = HashMap::new();
    // ── #15 char[]/byte[] waste, captured HERE (only place raw capacity is
    // visible). "used" = decoded byte length the String logically holds;
    // "capacity" = raw backing-array byte size; "wasted" = capacity - used.
    // Modern JDKs (7u6+ / compact strings 9+) usually size the array exactly,
    // so this is commonly 0 — but Java-8 char[] slack and any decode shrink
    // (e.g. a UTF-16BE array whose lossy-decoded UTF-8 is shorter) show up.
    // Retained candidates are bounded to CHAR_ARRAY_WASTE_TOP via a min-heap
    // keyed on wasted bytes (Reverse => smallest at top for cheap eviction),
    // so RSS stays bounded even with millions of wasteful arrays.
    let mut arrays_examined: u64 = 0;
    let mut wasteful_arrays: u64 = 0;
    let mut total_wasted_bytes: u64 = 0;
    // Heap entry ordering: (wasted, capacity, used, arr_addr). arr_addr breaks
    // ties deterministically; wrapped in Reverse so the *smallest* wasted is the
    // heap root and is evicted first.
    let mut waste_heap: std::collections::BinaryHeap<std::cmp::Reverse<(u64, u64, u64, u64)>> =
        std::collections::BinaryHeap::new();
    scan_prim_arrays(path, id_size, &wanted_arrays, |addr, bytes| {
        let coder = arr_coder.get(&addr).copied().unwrap_or(1);
        let decoded = decode_java_string(bytes, coder);
        let mut h = DefaultHasher::new();
        decoded.hash(&mut h);
        let hv = h.finish();
        let len = decoded.len() as u32;
        arr_hash.insert(addr, (hv, len));

        // #15 waste bookkeeping (bounded top-K).
        arrays_examined += 1;
        let capacity_bytes = bytes.len() as u64;
        let used_bytes = decoded.len() as u64;
        let wasted = capacity_bytes.saturating_sub(used_bytes);
        if wasted > 0 {
            wasteful_arrays += 1;
            total_wasted_bytes += wasted;
            waste_heap.push(std::cmp::Reverse((
                wasted,
                capacity_bytes,
                used_bytes,
                addr,
            )));
            if waste_heap.len() > CHAR_ARRAY_WASTE_TOP {
                waste_heap.pop(); // evict the smallest-wasted entry
            }
        }
    })?;

    // Materialize the #15 waste report from the bounded heap. Sort the retained
    // candidates by wasted DESC, tie-break array_obj_1based ASC (total order).
    let char_array_waste: Option<CharArrayWaste> = if arrays_examined == 0 {
        None
    } else {
        let mut rows: Vec<CharArrayWasteRow> = waste_heap
            .into_iter()
            .map(
                |std::cmp::Reverse((wasted, capacity_bytes, used_bytes, arr_addr))| {
                    // Dense 1-based object index; 0 if the addr somehow isn't mapped.
                    let array_obj_1based = p1.id_map.index_of(arr_addr).map(|i| i + 1).unwrap_or(0);
                    CharArrayWasteRow {
                        array_obj_1based,
                        length: capacity_bytes,
                        used: used_bytes,
                        wasted_bytes: wasted,
                    }
                },
            )
            .collect();
        rows.sort_unstable_by(|a, b| {
            b.wasted_bytes
                .cmp(&a.wasted_bytes)
                .then(a.array_obj_1based.cmp(&b.array_obj_1based))
        });
        rows.truncate(CHAR_ARRAY_WASTE_TOP);
        Some(CharArrayWaste {
            arrays_examined,
            wasteful_arrays,
            total_wasted_bytes,
            top: rows,
        })
    };
    // ── Fold: count per String INSTANCE by its array's value hash ────────────
    // dup_map: value_hash -> (count, len). hash_arr: value_hash -> one
    // representative array address (for later exact-text recovery of winners).
    let mut dup_map: HashMap<u64, (u32, u32)> = HashMap::new();
    let mut hash_arr: HashMap<u64, u64> = HashMap::new();
    for (arr_addr, _coder) in &per_instance {
        let Some(&(hv, len)) = arr_hash.get(arr_addr) else {
            continue;
        };
        let e = dup_map.entry(hv).or_insert((0, len));
        e.0 = e.0.saturating_add(1);
        hash_arr.entry(hv).or_insert(*arr_addr);
    }
    // Free the transient per-instance/per-array structures now that folding is
    // done. `arr_coder` is still needed for the winners' text-recovery pass.
    drop(per_instance);
    drop(arr_hash);

    // ── Summary + length histogram + length stats over DISTINCT values ───────
    let distinct_values = dup_map.len() as u64;
    let mut duplicated_values: u64 = 0;
    let mut approx_wasted_bytes: u64 = 0;
    // Power-of-two length buckets keyed by upper bound; also collect lengths for
    // min/max/median (bounded by #distinct values, already in RAM).
    let mut len_buckets: std::collections::BTreeMap<u32, u64> = std::collections::BTreeMap::new();
    let mut lengths: Vec<u32> = Vec::with_capacity(dup_map.len());
    let mut len_total: u64 = 0;
    for &(count, len) in dup_map.values() {
        if count > 1 {
            duplicated_values += 1;
            approx_wasted_bytes += (count as u64 - 1) * len as u64;
        }
        let upper = len.checked_next_power_of_two().unwrap_or(u32::MAX).max(1);
        *len_buckets.entry(upper).or_insert(0) += 1;
        lengths.push(len);
        len_total += len as u64;
    }
    let length_histogram: Vec<StrLenBucket> = len_buckets
        .into_iter()
        .map(|(upper_len, count)| StrLenBucket { upper_len, count })
        .collect();
    lengths.sort_unstable();
    let length_stats = if lengths.is_empty() {
        StrLenStats::default()
    } else {
        StrLenStats {
            min: lengths[0],
            max: lengths[lengths.len() - 1],
            median: lengths[lengths.len() / 2],
            total: len_total,
        }
    };

    // ── Select top-N most-duplicated values (count desc, hash asc) ───────────
    let mut ranked: Vec<(u64, u32, u32)> = dup_map
        .iter()
        .filter(|(_, (count, _))| *count > 1)
        .map(|(&hv, &(count, len))| (hv, count, len))
        .collect();
    ranked.sort_unstable_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    ranked.truncate(TOP_STRINGS_N);

    // ── #5 Select the LONGEST distinct values (len desc, hash asc). Unlike the
    // duplicate ranking this is NOT filtered by count>1 — the longest Strings
    // are interesting even when unique. Captured here (before the drops below)
    // so their representative arrays ride along in the single Pass-C scan.
    let mut ranked_by_len: Vec<(u64, u32, u32)> = dup_map
        .iter()
        .map(|(&hv, &(count, len))| (hv, count, len))
        .collect();
    ranked_by_len.sort_unstable_by(|a, b| b.2.cmp(&a.2).then(a.0.cmp(&b.0)));
    ranked_by_len.truncate(TOP_STRINGS_BY_LEN);

    // Map each winner's representative array address → its (hash, count, len).
    let mut winner_arr_meta: HashMap<u64, (u64, u32, u32)> = HashMap::new();
    for &(hv, count, len) in &ranked {
        if let Some(&arr_addr) = hash_arr.get(&hv) {
            winner_arr_meta.insert(arr_addr, (hv, count, len));
        }
    }
    // Fold the length-winners into the SAME recovery set so Pass C's one scan
    // recovers text for both rankings (no extra full-file scan).
    for &(hv, count, len) in &ranked_by_len {
        if let Some(&arr_addr) = hash_arr.get(&hv) {
            winner_arr_meta.insert(arr_addr, (hv, count, len));
        }
    }
    drop(dup_map);
    drop(hash_arr);

    // ── Pass C: recover exact text for ≤N winners only ───────────────────────
    // Only ≤TOP_STRINGS_N sample texts (each ≤MAX_STR_SAMPLE bytes) are ever
    // retained, so RSS stays bounded.
    let mut hash_text: HashMap<u64, String> = HashMap::new();
    if !winner_arr_meta.is_empty() {
        let winner_arrays: HashSet<u64> = winner_arr_meta.keys().copied().collect();
        scan_prim_arrays(path, id_size, &winner_arrays, |addr, bytes| {
            let Some(&(hv, _count, _len)) = winner_arr_meta.get(&addr) else {
                return;
            };
            let coder = arr_coder.get(&addr).copied().unwrap_or(1);
            let mut decoded = decode_java_string(bytes, coder);
            truncate_on_char_boundary(&mut decoded, MAX_STR_SAMPLE);
            hash_text.insert(hv, decoded);
        })?;
    }
    drop(arr_coder);
    let top_duplicated: Vec<DupStringSample> = ranked
        .iter()
        .map(|&(hv, count, len)| DupStringSample {
            text: hash_text.get(&hv).cloned().unwrap_or_default(),
            count: count as u64,
            len,
            wasted_bytes: (count as u64 - 1) * len as u64,
        })
        .collect();
    // #5: same text-recovery pattern, from the length ranking. wasted_bytes uses
    // the same (count-1)*len formula (0 for unique values, where count==1).
    let top_by_length: Vec<DupStringSample> = ranked_by_len
        .iter()
        .map(|&(hv, count, len)| DupStringSample {
            text: hash_text.get(&hv).cloned().unwrap_or_default(),
            count: count as u64,
            len,
            wasted_bytes: (count as u64).saturating_sub(1) * len as u64,
        })
        .collect();
    drop(hash_text);

    // ── Pass D: classes holding the most Strings ─────────────────────────────
    // Walk EVERY instance's object-reference fields via the memoized FieldPlan;
    // credit each owning class for every field ref that points at a String
    // instance. Bounded by #classes (the counter) + string_addrs (already held).
    let top_string_holders =
        compute_string_holders(path, p1, &string_addrs, id_size, obj_ref_width)?;
    drop(string_addrs);

    Ok(DupStrings {
        distinct_values,
        duplicated_values,
        total_string_instances,
        approx_wasted_bytes,
        top_duplicated,
        length_histogram,
        length_stats,
        top_string_holders,
        top_by_length,
        char_array_waste,
    })
}

/// Truncate `s` in place to at most `max_bytes` bytes, respecting UTF-8 char
/// boundaries (never splits a codepoint).
pub(crate) fn truncate_on_char_boundary(s: &mut String, max_bytes: usize) {
    if s.len() <= max_bytes {
        return;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s.truncate(end);
}

/// Walk every INSTANCE_DUMP's object-reference fields (via the memoized
/// per-class `FieldPlan`) and, for each reference that points at a
/// `java.lang.String` instance, credit the owning (referencing) class. Returns
/// the top-N owning classes by String-reference count. RSS is bounded by the
/// per-class counter (#classes) plus `string_addrs` (already held by the
/// caller); no per-object state is retained.
pub(crate) fn compute_string_holders(
    path: &str,
    p1: &Pass1,
    string_addrs: &std::collections::HashSet<u64>,
    id_size: u8,
    obj_ref_width: usize,
) -> io::Result<Vec<StringHolder>> {
    let class_map = &p1.class_map;
    let strings = &p1.strings;
    let field_plans = build_field_plans(class_map, strings, id_size as usize);
    // owning class address → count of String-instance references.
    let mut class_counter: HashMap<u64, u64> = HashMap::new();

    scan_all_instances(path, id_size, |_obj_addr, class_id, blob| {
        let Some(plan) = field_plans.get(&class_id) else {
            return;
        };
        let mut hits: u64 = 0;
        for &(offset, _excluded) in plan {
            let off = offset as usize;
            if off + obj_ref_width > blob.len() {
                continue;
            }
            let r = read_ref(&blob[off..], obj_ref_width);
            if r != 0 && string_addrs.contains(&r) {
                hits += 1;
            }
        }
        if hits > 0 {
            *class_counter.entry(class_id).or_insert(0) += hits;
        }
    })?;
    drop(field_plans);

    // Resolve names and rank (refs desc, name asc). Bounded by #classes.
    let mut holders: Vec<StringHolder> = class_counter
        .into_iter()
        .map(|(class_addr, string_refs)| {
            let class_name = class_map
                .get(&class_addr)
                .and_then(|ci| strings.get(&ci.name_id))
                .map(|s| s.replace('/', "."))
                .unwrap_or_else(|| format!("0x{class_addr:x}"));
            StringHolder {
                class_name,
                string_refs,
            }
        })
        .collect();
    holders.sort_unstable_by(|a, b| {
        b.string_refs
            .cmp(&a.string_refs)
            .then(a.class_name.cmp(&b.class_name))
    });
    holders.truncate(TOP_STRINGS_N);
    Ok(holders)
}

// ── Utility ────────────────────────────────────────────────────────────────

/// Decode the backing element bytes of a `java.lang.String` into a Rust
/// `String`. `coder` follows the JDK 9+ `String.coder` convention:
///
/// - `0` = LATIN1: one byte per char, interpreted as ISO-8859-1.
/// - `1` = UTF16: two bytes per char, big-endian (HPROF byte order).
///
/// A JDK 8 `char[] value` has no `coder` field; callers pass `coder == 1`
/// because HPROF stores its chars as big-endian UTF-16 code units. Any other
/// `coder` value is treated as UTF16 (the only multi-byte case). Reusable by
/// later String-decoding stages.
pub fn decode_java_string(bytes: &[u8], coder: u8) -> String {
    if coder == 0 {
        // LATIN1 / ISO-8859-1: each byte is a Unicode code point 0..=255.
        bytes.iter().map(|&b| b as char).collect()
    } else {
        // UTF-16BE: pair bytes big-endian, lossily decode surrogates.
        let units: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
            .collect();
        String::from_utf16_lossy(&units)
    }
}

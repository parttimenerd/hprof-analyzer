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
    StringHolder, field_offset, scan_prim_arrays, skip_class_dump, sub_remaining,
};

/// Intermediate state for dup-strings Pass B, populated inline during the 2b
/// forward-CSR fill instead of running a separate `scan_prim_arrays` pass.
/// Call `on_prim_array` for each PRIM_ARRAY_DUMP encountered in the 2b scan,
/// then `finish` to produce the final `DupStrings`.
pub(crate) struct DupStringPassB {
    /// arr_addr → coder (first-seen per backing array).
    pub(crate) arr_coder: HashMap<u64, u8>,
    /// arr_addr → (value_hash, decoded_len) — populated by `on_prim_array`.
    pub(crate) arr_hash: HashMap<u64, (u64, u32)>,
    /// #15 char[]/byte[] waste counters.
    arrays_examined: u64,
    wasteful_arrays: u64,
    total_wasted_bytes: u64,
    /// Bounded min-heap: (wasted, capacity, used, arr_addr). Reverse so the
    /// smallest-wasted is at the top (cheapest to evict when over capacity).
    waste_heap: std::collections::BinaryHeap<std::cmp::Reverse<(u64, u64, u64, u64)>>,
    /// Per-instance list (arr_addr, coder) — needed for the Fold step.
    pub(crate) per_instance: Vec<(u64, u8)>,
    /// Every java.lang.String instance address (for Pass D holder counting).
    pub(crate) string_addrs: std::collections::HashSet<u64>,
    pub(crate) total_string_instances: u64,
}

impl DupStringPassB {
    /// Process one PRIM_ARRAY_DUMP seen during the 2b fill.  Only acts when
    /// `addr` is in `arr_coder`; otherwise returns immediately (no work).
    pub(crate) fn on_prim_array(&mut self, addr: u64, bytes: &[u8]) {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let coder = match self.arr_coder.get(&addr).copied() {
            Some(c) => c,
            None => return, // not a String backing array
        };
        let decoded = decode_java_string(bytes, coder);
        let mut h = DefaultHasher::new();
        decoded.hash(&mut h);
        let hv = h.finish();
        let len = decoded.len() as u32;
        self.arr_hash.insert(addr, (hv, len));

        // #15 waste bookkeeping (bounded top-K).
        self.arrays_examined += 1;
        let capacity_bytes = bytes.len() as u64;
        let used_bytes = decoded.len() as u64;
        let wasted = capacity_bytes.saturating_sub(used_bytes);
        if wasted > 0 {
            self.wasteful_arrays += 1;
            self.total_wasted_bytes += wasted;
            self.waste_heap.push(std::cmp::Reverse((
                wasted,
                capacity_bytes,
                used_bytes,
                addr,
            )));
            if self.waste_heap.len() > CHAR_ARRAY_WASTE_TOP {
                self.waste_heap.pop();
            }
        }
    }

    /// Finish Pass B state into a `DupStrings`.  Runs the Fold, summary stats,
    /// ranking, and Pass C (winners-only text recovery).  Equivalent to the
    /// tail of the old `resolve_duplicate_strings` after the `scan_prim_arrays`
    /// call.  `p1` is needed only for `id_map` (waste row addr→dense-idx).
    pub(crate) fn finish<O>(self, p1: &Pass1, open: O) -> io::Result<DupStrings>
    where
        O: Fn() -> io::Result<HprofReader>,
    {
        use std::collections::HashSet;

        let DupStringPassB {
            arr_coder,
            arr_hash,
            arrays_examined,
            wasteful_arrays,
            total_wasted_bytes,
            waste_heap,
            per_instance,
            string_addrs: _,
            total_string_instances,
        } = self;

        let id_size = p1.id_size;

        // Materialize char_array_waste from the bounded heap.
        let char_array_waste: Option<CharArrayWaste> = if arrays_examined == 0 {
            None
        } else {
            let mut rows: Vec<CharArrayWasteRow> = waste_heap
                .into_iter()
                .map(
                    |std::cmp::Reverse((wasted, capacity_bytes, used_bytes, arr_addr))| {
                        let array_obj_1based =
                            p1.id_map.index_of(arr_addr).map(|i| i + 1).unwrap_or(0);
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

        // ── Fold: count per instance by its array's value hash ────────────────
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
        drop(per_instance);
        drop(arr_hash);

        // ── Summary + histogram ───────────────────────────────────────────────
        let distinct_values = dup_map.len() as u64;
        let mut duplicated_values: u64 = 0;
        let mut approx_wasted_bytes: u64 = 0;
        let mut len_buckets: std::collections::BTreeMap<u32, u64> =
            std::collections::BTreeMap::new();
        let mut lengths: Vec<u32> = Vec::with_capacity(dup_map.len());
        let mut len_total: u64 = 0;
        for &(count, len) in dup_map.values() {
            if count > 1 {
                duplicated_values += 1;
                approx_wasted_bytes = approx_wasted_bytes
                    .saturating_add((count as u64).saturating_sub(1).saturating_mul(len as u64));
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

        // ── Select top-N winners ──────────────────────────────────────────────
        let mut ranked: Vec<(u64, u32, u32)> = dup_map
            .iter()
            .filter(|(_, (count, _))| *count > 1)
            .map(|(&hv, &(count, len))| (hv, count, len))
            .collect();
        ranked.sort_unstable_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        ranked.truncate(TOP_STRINGS_N);

        let mut ranked_by_len: Vec<(u64, u32, u32)> = dup_map
            .iter()
            .map(|(&hv, &(count, len))| (hv, count, len))
            .collect();
        ranked_by_len.sort_unstable_by(|a, b| b.2.cmp(&a.2).then(a.0.cmp(&b.0)));
        ranked_by_len.truncate(TOP_STRINGS_BY_LEN);

        let mut winner_arr_meta: HashMap<u64, (u64, u32, u32)> = HashMap::new();
        for &(hv, count, len) in &ranked {
            if let Some(&arr_addr) = hash_arr.get(&hv) {
                winner_arr_meta.insert(arr_addr, (hv, count, len));
            }
        }
        for &(hv, count, len) in &ranked_by_len {
            if let Some(&arr_addr) = hash_arr.get(&hv) {
                winner_arr_meta.insert(arr_addr, (hv, count, len));
            }
        }
        drop(dup_map);
        drop(hash_arr);

        // ── Pass C: recover exact text for ≤N winners only ───────────────────
        let mut hash_text: HashMap<u64, String> = HashMap::new();
        if !winner_arr_meta.is_empty() {
            let winner_arrays: HashSet<u64> = winner_arr_meta.keys().copied().collect();
            scan_prim_arrays(&open, id_size, &winner_arrays, |addr, bytes| {
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
                wasted_bytes: (count as u64).saturating_sub(1).saturating_mul(len as u64),
            })
            .collect();
        let top_by_length: Vec<DupStringSample> = ranked_by_len
            .iter()
            .map(|&(hv, count, len)| DupStringSample {
                text: hash_text.get(&hv).cloned().unwrap_or_default(),
                count: count as u64,
                len,
                wasted_bytes: (count as u64).saturating_sub(1).saturating_mul(len as u64),
            })
            .collect();
        drop(hash_text);

        Ok(DupStrings {
            distinct_values,
            duplicated_values,
            total_string_instances,
            approx_wasted_bytes,
            top_duplicated,
            length_histogram,
            length_stats,
            top_string_holders: Vec::new(),
            top_by_length,
            char_array_waste,
        })
    }
}

/// Build a `DupStringPassB` collector from the triples captured during the 2a
/// heap walk.  Pure computation — no file I/O.  The caller must then feed
/// every PRIM_ARRAY_DUMP encountered in the 2b forward-CSR fill to
/// `collector.on_prim_array`, and call `collector.finish` after the 2b scan
/// to produce the final `DupStrings`.  Returns `None` (and an empty set) when
/// `captured` is empty (no java.lang.String instances found).
pub(crate) fn prepare_dup_strings(captured: Vec<(u64, u64, u8)>) -> Option<DupStringPassB> {
    use std::collections::HashSet;

    if captured.is_empty() {
        return None;
    }

    let total_string_instances = captured.len() as u64;
    let mut per_instance: Vec<(u64, u8)> = Vec::with_capacity(captured.len());
    let mut arr_coder: HashMap<u64, u8> = HashMap::new();
    let mut string_addrs: HashSet<u64> = HashSet::with_capacity(captured.len());

    for (obj_addr, arr_addr, coder) in captured {
        per_instance.push((arr_addr, coder));
        arr_coder.entry(arr_addr).or_insert(coder);
        string_addrs.insert(obj_addr);
    }

    Some(DupStringPassB {
        arr_coder,
        arr_hash: HashMap::new(),
        arrays_examined: 0,
        wasteful_arrays: 0,
        total_wasted_bytes: 0,
        waste_heap: std::collections::BinaryHeap::new(),
        per_instance,
        string_addrs,
        total_string_instances,
    })
}

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
pub(crate) fn scan_all_instances<O, F>(open: O, id_size: u8, mut f: F) -> io::Result<()>
where
    O: Fn() -> io::Result<HprofReader>,
    F: FnMut(u64, u64, &[u8]),
{
    let ids = id_size as u64;
    let mut r = open()?;
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
                    sub_remaining(&mut remaining, 1)?;
                    match sub_tag {
                        heap::ROOT_SYSTEM_CLASS
                        | heap::ROOT_UNKNOWN
                        | heap::ROOT_MONITOR_USED
                        | heap::ROOT_STICKY_CLASS
                        | heap::ROOT_INTERNED_STRING
                        | heap::ROOT_DEBUGGER
                        | heap::ROOT_VM_INTERNAL => {
                            r.skip(ids)?;
                            sub_remaining(&mut remaining, ids)?;
                        }
                        heap::ROOT_JNI_GLOBAL => {
                            r.skip(2 * ids)?;
                            sub_remaining(&mut remaining, 2 * ids)?;
                        }
                        heap::ROOT_JNI_LOCAL
                        | heap::ROOT_JAVA_FRAME
                        | heap::ROOT_JNI_MONITOR
                        | heap::ROOT_THREAD_OBJ => {
                            r.skip(ids + 8)?;
                            sub_remaining(&mut remaining, ids + 8)?;
                        }
                        heap::ROOT_NATIVE_STACK | heap::ROOT_THREAD_BLOCK => {
                            r.skip(ids + 4)?;
                            sub_remaining(&mut remaining, ids + 4)?;
                        }
                        heap::HEAP_DUMP_INFO => {
                            r.skip(4 + ids)?;
                            sub_remaining(&mut remaining, 4 + ids)?;
                        }
                        heap::CLASS_DUMP => {
                            let consumed = skip_class_dump(&mut r, id_size)?;
                            sub_remaining(&mut remaining, consumed)?;
                        }
                        heap::INSTANCE_DUMP => {
                            let addr = r.id()?;
                            r.skip(4)?;
                            let class_id = r.id()?;
                            let data_len = r.u4()? as u64;
                            sub_remaining(&mut remaining, ids + 4 + ids + 4 + data_len)?;
                            r.read_bytes_reuse(&mut scratch, data_len as usize)?;
                            f(addr, class_id, &scratch);
                        }
                        heap::OBJ_ARRAY_DUMP => {
                            r.skip(ids + 4)?;
                            let count = r.u4()? as u64;
                            r.skip(ids)?;
                            let byte_len = count.saturating_mul(ids);
                            r.skip(byte_len)?;
                            sub_remaining(&mut remaining, ids + 4 + 4 + ids + byte_len)?;
                        }
                        heap::PRIM_ARRAY_NODATA_DUMP => {
                            // Android ART: same header as PRIM_ARRAY_DUMP but no element data.
                            r.skip(ids + 4 + 4 + 1)?;
                            sub_remaining(&mut remaining, ids + 4 + 4 + 1)?;
                        }

                        heap::PRIM_ARRAY_DUMP => {
                            r.skip(ids + 4)?;
                            let count = r.u4()? as u64;
                            let elem_type = r.u1()?;
                            let esz = HprofType::from_code(elem_type)
                                .map(|t| t.byte_size() as u64)
                                .unwrap_or(1);
                            r.skip(count.saturating_mul(esz))?;
                            sub_remaining(
                                &mut remaining,
                                ids + 4 + 4 + 1 + count.saturating_mul(esz),
                            )?;
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

/// Pre-compute the java.lang.String class address + field offsets for use
/// during the 2a scan (so Pass A of resolve_duplicate_strings can be folded
/// into the 2a heap walk instead of running a separate scan_all_instances).
///
/// Returns `(class_id, value_off, coder_off)` for the first String class found,
/// or `None` if java.lang.String is absent from the class_map.
/// `value_off` is the byte offset of the `value` ref field within the instance
/// blob; `coder_off` is the byte offset of the `coder` byte field (None on
/// Java 8 char[] layout — treat as coder=1).
pub(crate) fn string_class_info(p1: &Pass1) -> Option<(u64, usize, Option<usize>)> {
    let obj_ref_width = p1.id_size as usize;
    // Find the class addr for java.lang.String.
    let string_class_id = p1.class_map.iter().find_map(|(&addr, ci)| {
        p1.strings
            .get(&ci.name_id)
            .filter(|name| *name == "java/lang/String")
            .map(|_| addr)
    })?;
    let value_off = match field_offset(
        string_class_id,
        "value",
        "java/lang/String",
        &p1.class_map,
        &p1.strings,
        obj_ref_width,
    ) {
        Some((off, HprofType::Object)) => off as usize,
        _ => return None,
    };
    let coder_off = match field_offset(
        string_class_id,
        "coder",
        "java/lang/String",
        &p1.class_map,
        &p1.strings,
        obj_ref_width,
    ) {
        Some((off, HprofType::Byte)) => Some(off as usize),
        _ => None,
    };
    Some((string_class_id, value_off, coder_off))
}

/// Rank the String-holder counts accumulated during the 2b scan into the top-N
/// owning classes (refs desc, name asc). This is the tail of the old
/// `compute_string_holders`, split out so the per-instance ref-counting can be
/// folded into the 2b forward-CSR fill instead of running a dedicated scan.
/// `class_counter` maps owning class ADDRESS → String-instance reference count.
/// `name_of` resolves a class address to its dotted name (the caller supplies a
/// pre-free snapshot, since class_map/strings are freed before the 2b scan).
pub(crate) fn finalize_string_holders(
    class_counter: HashMap<u64, u64>,
    name_of: impl Fn(u64) -> String,
) -> Vec<StringHolder> {
    let mut holders: Vec<StringHolder> = class_counter
        .into_iter()
        .map(|(class_addr, string_refs)| StringHolder {
            class_name: name_of(class_addr),
            string_refs,
        })
        .collect();
    holders.sort_unstable_by(|a, b| {
        b.string_refs
            .cmp(&a.string_refs)
            .then(a.class_name.cmp(&b.class_name))
    });
    holders.truncate(TOP_STRINGS_N);
    holders
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

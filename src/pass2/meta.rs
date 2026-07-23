//! Pass-2 thread / alloc-site / system-property resolution + frame formatting.

use std::collections::HashMap;

use std::io;

use crate::{pass1::Pass1, types::HprofType};

use super::{
    ThreadProps, ThreadStack, collect_blobs, decode_java_string, field_offset, read_ref,
    scan_class_dumps,
};

/// Cached byte offsets of the `(daemon, priority, threadStatus)` fields within a
/// `java.lang.Thread$FieldHolder` instance blob, keyed by concrete class id.
/// `None` marks a field absent on that layout.
type HolderOffsets = (Option<usize>, Option<usize>, Option<usize>);

/// Resolve pass1's STACK_TRACE/STACK_FRAME tables into pre-rendered thread
/// stacks. Each frame becomes `class.method (source:line)`; unresolved string
/// ids fall back to their hex id, unknown/negative line numbers are rendered
/// per HPROF convention. Traces with no frames are dropped. Output is sorted by
/// `thread_serial` for determinism. Small (one entry per thread trace).
pub(crate) fn build_thread_stacks(p1: &Pass1) -> Vec<ThreadStack> {
    let resolve = |id: u64| -> Option<&str> { p1.strings.get(&id).map(|s| s.as_str()) };
    let class_name_of = |serial: u32| -> Option<&str> {
        let addr = *p1.class_serial_to_addr.get(&serial)?;
        let ci = p1.class_map.get(&addr)?;
        p1.strings.get(&ci.name_id).map(|s| s.as_str())
    };

    let mut out: Vec<ThreadStack> = Vec::new();
    for (&stack_serial, frame_ids) in p1.stack_traces.iter() {
        if frame_ids.is_empty() {
            continue;
        }
        let thread_serial = p1
            .stack_trace_thread
            .get(&stack_serial)
            .copied()
            .unwrap_or(0);
        let thread_obj_idx = p1
            .thread_serial_to_obj_id
            .get(&thread_serial)
            .and_then(|&addr| p1.id_map.index_of(addr))
            .map(|i| i as u32)
            .unwrap_or(u32::MAX);

        let mut frames = Vec::with_capacity(frame_ids.len());
        for &fid in frame_ids {
            let Some(f) = p1.stack_frames.get(&fid) else {
                frames.push(format!("<unknown frame {fid:#x}>"));
                continue;
            };
            let class = class_name_of(f.class_serial).map(pretty_binary_name);
            let method = resolve(f.method_name_id);
            let source = resolve(f.source_file_id);
            frames.push(render_frame(
                class.as_deref(),
                method,
                source,
                f.class_serial,
                f.line_number,
            ));
        }
        out.push(ThreadStack {
            thread_serial,
            thread_obj_idx,
            frames,
        });
    }
    out.sort_by_key(|t| t.thread_serial);
    out
}

/// Pre-resolve the DISTINCT non-zero alloc stack-trace serials appearing in
/// `p1.alloc_stack_serial` into their rendered frame lines, using the same
/// STACK_TRACE/STACK_FRAME + string/class machinery as `build_thread_stacks`.
/// Called only when `--alloc-sites` is on, while those tables are still alive.
/// Bounded by the number of distinct traces (hundreds), so it stays off the
/// per-object RSS budget. A serial with no STACK_TRACE record maps to an empty
/// frame Vec.
pub(crate) fn resolve_alloc_frames(p1: &Pass1) -> std::collections::HashMap<u32, Vec<String>> {
    let resolve = |id: u64| -> Option<&str> { p1.strings.get(&id).map(|s| s.as_str()) };
    let class_name_of = |serial: u32| -> Option<&str> {
        let addr = *p1.class_serial_to_addr.get(&serial)?;
        let ci = p1.class_map.get(&addr)?;
        p1.strings.get(&ci.name_id).map(|s| s.as_str())
    };

    // Collect the distinct non-zero serials first (bounded, dedup via HashSet).
    let mut distinct: std::collections::HashSet<u32> = std::collections::HashSet::new();
    for &s in &p1.alloc_stack_serial {
        if s != 0 {
            distinct.insert(s);
        }
    }

    let mut map: std::collections::HashMap<u32, Vec<String>> =
        std::collections::HashMap::with_capacity(distinct.len());
    for &serial in &distinct {
        let frames = match p1.stack_traces.get(&serial) {
            Some(frame_ids) => {
                let mut frames = Vec::with_capacity(frame_ids.len());
                for &fid in frame_ids {
                    let Some(f) = p1.stack_frames.get(&fid) else {
                        frames.push(format!("<unknown frame {fid:#x}>"));
                        continue;
                    };
                    let class = class_name_of(f.class_serial).map(pretty_binary_name);
                    let method = resolve(f.method_name_id);
                    let source = resolve(f.source_file_id);
                    frames.push(render_frame(
                        class.as_deref(),
                        method,
                        source,
                        f.class_serial,
                        f.line_number,
                    ));
                }
                frames
            }
            None => Vec::new(),
        };
        map.insert(serial, frames);
    }
    map
}

/// Decode each thread's `java.lang.Thread` properties into a `ThreadProps` via a
/// bounded multi-pass worklist: the `name` String (decoded to UTF-8) plus the
/// always-on overview scalars (daemon / priority / threadStatus /
/// contextClassLoader address) read straight from the same thread blob. All
/// captured sets are bounded by the number of threads (hundreds) and the tiny
/// Strings/arrays they reference, so this stays off the per-object RSS budget
/// even on multi-GB dumps.
///
/// `prefetched_thread_blobs`: instance blobs for thread objects captured during
/// the 2a scan (addr → (class_id, blob)). When provided, skips the separate
/// Round-1 file pass entirely. Remaining hops (String objects, backing arrays)
/// still need 2 targeted collect_blobs calls.
///
/// Field offsets are derived from each object's ACTUAL class id (memoized),
/// because a heap may hold several loader-distinct class objects named
/// `java/lang/Thread` / `java/lang/String`, and thread objects are frequently
/// subclasses whose inherited `name` sits past the subclass's own fields.
pub(crate) fn resolve_thread_names(
    path: &str,
    p1: &Pass1,
    prefetched_thread_blobs: HashMap<u64, (u64, Vec<u8>)>,
) -> io::Result<HashMap<u32, ThreadProps>> {
    let mut props: HashMap<u32, ThreadProps> = HashMap::new();
    if p1.thread_serial_to_obj_id.is_empty() {
        return Ok(props);
    }
    let id_size = p1.id_size;
    // Object references inside an INSTANCE_DUMP blob are always id_size wide.
    let obj_ref_width = id_size as usize;
    let class_map = &p1.class_map;
    let strings = &p1.strings;

    let read_i32 = |blob: &[u8], o: usize| -> Option<i32> {
        blob.get(o..o + 4)
            .map(|b| i32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    };

    // ── Round 1: Thread blobs ────────────────────────────────────────────────
    // Use pre-fetched blobs captured during the 2a scan when available; fall
    // back to a targeted collect_blobs call only for any missing entries.
    let mut inst_blobs_r1 = prefetched_thread_blobs;
    let missing_threads: std::collections::HashSet<u64> = p1
        .thread_serial_to_obj_id
        .values()
        .copied()
        .filter(|a| !inst_blobs_r1.contains_key(a))
        .collect();
    if !missing_threads.is_empty() {
        let (extra, _, _) = collect_blobs(
            path,
            id_size,
            &missing_threads,
            &std::collections::HashSet::new(),
            &std::collections::HashSet::new(),
        )?;
        inst_blobs_r1.extend(extra);
    }

    // Extract thread fields from round-1 blobs.
    let mut thread_to_name_addr: HashMap<u64, u64> = HashMap::new();
    let mut thread_to_scalars: HashMap<u64, (bool, i32, i32, u64)> = HashMap::new();
    let mut thread_to_holder: HashMap<u64, u64> = HashMap::new();
    // class_id → (name_off, daemon_off, priority_off, status_off, ctx_off, holder_off)
    type ThreadOffs = (
        Option<usize>,
        Option<usize>,
        Option<usize>,
        Option<usize>,
        Option<usize>,
        Option<usize>,
    );
    let mut off_cache: HashMap<u64, ThreadOffs> = HashMap::new();

    for (&addr, &(class_id, ref blob)) in &inst_blobs_r1 {
        let offs = *off_cache.entry(class_id).or_insert_with(|| {
            let obj_off = |name: &str| match field_offset(
                class_id,
                name,
                "java/lang/Thread",
                class_map,
                strings,
                obj_ref_width,
            ) {
                Some((off, HprofType::Object)) => Some(off as usize),
                _ => None,
            };
            let name_off = obj_off("name");
            let daemon_off = match field_offset(
                class_id,
                "daemon",
                "java/lang/Thread",
                class_map,
                strings,
                obj_ref_width,
            ) {
                Some((off, HprofType::Boolean)) => Some(off as usize),
                _ => None,
            };
            let priority_off = match field_offset(
                class_id,
                "priority",
                "java/lang/Thread",
                class_map,
                strings,
                obj_ref_width,
            ) {
                Some((off, HprofType::Int)) => Some(off as usize),
                _ => None,
            };
            let status_off = match field_offset(
                class_id,
                "threadStatus",
                "java/lang/Thread",
                class_map,
                strings,
                obj_ref_width,
            ) {
                Some((off, HprofType::Int)) => Some(off as usize),
                _ => None,
            };
            let ctx_off = obj_off("contextClassLoader");
            let holder_off = obj_off("holder");
            (name_off, daemon_off, priority_off, status_off, ctx_off, holder_off)
        });
        let (name_off, daemon_off, priority_off, status_off, ctx_off, holder_off) = offs;
        if let Some(off) = name_off {
            if off + obj_ref_width <= blob.len() {
                let name_ref = read_ref(&blob[off..], obj_ref_width);
                if name_ref != 0 {
                    thread_to_name_addr.insert(addr, name_ref);
                }
            }
        }
        let is_daemon = daemon_off.and_then(|o| blob.get(o)).map(|&b| b != 0).unwrap_or(false);
        let priority = priority_off.and_then(|o| read_i32(blob, o)).unwrap_or(0);
        let thread_status = status_off.and_then(|o| read_i32(blob, o)).unwrap_or(0);
        let context_loader_addr = ctx_off
            .filter(|&o| o + obj_ref_width <= blob.len())
            .map(|o| read_ref(&blob[o..], obj_ref_width))
            .unwrap_or(0);
        thread_to_scalars.insert(addr, (is_daemon, priority, thread_status, context_loader_addr));
        // Record the holder addr only when the scalars are NOT directly on Thread
        // (i.e. the FieldHolder layout), so the extra work is skipped for JDK 8-16.
        if priority_off.is_none() && daemon_off.is_none() && status_off.is_none() {
            if let Some(off) = holder_off {
                if off + obj_ref_width <= blob.len() {
                    let href = read_ref(&blob[off..], obj_ref_width);
                    if href != 0 {
                        thread_to_holder.insert(addr, href);
                    }
                }
            }
        }
    }
    drop(inst_blobs_r1);

    // ── Round 2: String objects + holder objects (JDK 17+) + backing arrays ──
    // Fuse into one pass: wanted_inst = name Strings ∪ holder objects,
    // wanted_prim = backing char[]/byte[] arrays.
    // String → array addrs are unknown until we read the String blobs; we solve
    // this by collecting String blobs first (in this same pass via wanted_inst),
    // then deriving array addrs in-memory before the pass returns.
    // Because wanted_prim is array addrs (only known after String blobs),
    // we need a second inner pass for arrays if there are name strings.
    // But we can fuse holder collection with String collection in one pass,
    // and fuse array collection with... nothing (arrays need String addrs first).
    // Net result: holders + Strings in one pass, then arrays in one pass = 2 passes
    // instead of the old 3 passes.
    let wanted_strings: std::collections::HashSet<u64> =
        thread_to_name_addr.values().copied().collect();
    let wanted_holders: std::collections::HashSet<u64> =
        thread_to_holder.values().copied().collect();
    let wanted_inst_r2: std::collections::HashSet<u64> =
        wanted_strings.iter().chain(wanted_holders.iter()).copied().collect();

    let mut string_to_arr: HashMap<u64, (u64, u8)> = HashMap::new();
    let mut holder_scalars: HashMap<u64, (bool, i32, i32)> = HashMap::new();

    if !wanted_inst_r2.is_empty() {
        let (inst_blobs_r2, _, _) = collect_blobs(
            path,
            id_size,
            &wanted_inst_r2,
            &std::collections::HashSet::new(),
            &std::collections::HashSet::new(),
        )?;

        // class_id → (value_off, coder_off) for String
        let mut str_off_cache: HashMap<u64, Option<(usize, Option<usize>)>> = HashMap::new();
        // class_id → (daemon_off, priority_off, status_off) for FieldHolder
        let mut holder_off_cache: HashMap<u64, HolderOffsets> = HashMap::new();

        for (&addr, &(class_id, ref blob)) in &inst_blobs_r2 {
            if wanted_strings.contains(&addr) {
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
                if let Some((value_off, coder_off)) = offs {
                    if value_off + obj_ref_width <= blob.len() {
                        let arr_ref = read_ref(&blob[value_off..], obj_ref_width);
                        let coder = match coder_off {
                            Some(co) if co < blob.len() => blob[co],
                            _ => 1, // Java 8 char[]: no coder field → UTF16
                        };
                        if arr_ref != 0 {
                            string_to_arr.insert(addr, (arr_ref, coder));
                        }
                    }
                }
            } else if wanted_holders.contains(&addr) {
                let (daemon_off, priority_off, status_off) =
                    *holder_off_cache.entry(class_id).or_insert_with(|| {
                        let int_off = |name: &str| match field_offset(
                            class_id,
                            name,
                            "java/lang/Thread$FieldHolder",
                            class_map,
                            strings,
                            obj_ref_width,
                        ) {
                            Some((off, HprofType::Int)) => Some(off as usize),
                            _ => None,
                        };
                        let daemon_off = match field_offset(
                            class_id,
                            "daemon",
                            "java/lang/Thread$FieldHolder",
                            class_map,
                            strings,
                            obj_ref_width,
                        ) {
                            Some((off, HprofType::Boolean)) => Some(off as usize),
                            _ => None,
                        };
                        (daemon_off, int_off("priority"), int_off("threadStatus"))
                    });
                let is_daemon = daemon_off.and_then(|o| blob.get(o)).map(|&b| b != 0).unwrap_or(false);
                let priority = priority_off.and_then(|o| read_i32(blob, o)).unwrap_or(0);
                let thread_status = status_off.and_then(|o| read_i32(blob, o)).unwrap_or(0);
                holder_scalars.insert(addr, (is_daemon, priority, thread_status));
            }
        }
    }

    // Fold holder scalars back into thread_to_scalars.
    for (&thread_addr, &holder_addr) in &thread_to_holder {
        if let Some(&(d, p, s)) = holder_scalars.get(&holder_addr) {
            if let Some(entry) = thread_to_scalars.get_mut(&thread_addr) {
                entry.0 = d;
                entry.1 = p;
                entry.2 = s;
            }
        }
    }

    // Seed props with scalar overview fields.
    for (&serial, &thread_addr) in &p1.thread_serial_to_obj_id {
        if let Some(&(is_daemon, priority, thread_status, ctx)) =
            thread_to_scalars.get(&thread_addr)
        {
            props.insert(
                serial,
                ThreadProps {
                    name: String::new(),
                    is_daemon,
                    priority,
                    thread_status,
                    context_loader_addr: ctx,
                },
            );
        }
    }
    if string_to_arr.is_empty() {
        return Ok(props);
    }

    // ── Round 3: backing PRIM_ARRAYs ─────────────────────────────────────────
    let wanted_arrays: std::collections::HashSet<u64> =
        string_to_arr.values().map(|&(a, _)| a).collect();
    let (_, arr_blobs, _) = collect_blobs(
        path,
        id_size,
        &std::collections::HashSet::new(),
        &wanted_arrays,
        &std::collections::HashSet::new(),
    )?;

    // ── Decode: serial → thread → String → array → text ──────────────────────
    for (&serial, &thread_addr) in &p1.thread_serial_to_obj_id {
        let Some(&name_addr) = thread_to_name_addr.get(&thread_addr) else {
            continue;
        };
        let Some(&(arr_addr, coder)) = string_to_arr.get(&name_addr) else {
            continue;
        };
        let Some(bytes) = arr_blobs.get(&arr_addr) else {
            continue;
        };
        let text = decode_java_string(bytes, coder);
        if !text.is_empty() {
            props.entry(serial).or_default().name = text;
        }
    }

    Ok(props)
}

/// Maximum number of system-property entries captured. The props table is ONE
/// object, but its slot count is attacker/dump-controlled, so every worklist
/// derived from it is capped at this bound to keep RSS bounded regardless of
/// dump size.
pub(crate) const MAX_PROP_ENTRIES: usize = 4096;

/// Sorted `(key, value)` system-property pairs plus the derived JVM version.
pub(crate) type SystemProps = (Vec<(String, String)>, Option<String>);

/// Capture java.lang.System's static `props` object and decode it into a sorted
/// (key, value) list of system properties plus a derived JVM version.
///
/// Strategy (all passes bounded — see `MAX_PROP_ENTRIES`):
///   P0: scan CLASS_DUMP records for the class named `java/lang/System`; read
///       its static object field `props` → the props object address.
///   Round 1 (collect_blobs): props instance → its Hashtable `table` Object[]
///       array address (one instance + one obj-array collected together).
///   Round 2 (collect_blobs): ALL Hashtable$Entry instances (from table slots)
///       + all key/value String instances + all backing PRIM_ARRAYs, collected
///       in ONE pass. Entry chain traversal happens in-memory after this pass,
///       so no iterative file scans for chains are needed.
///
/// Returns `(sorted (key,value) pairs, jvm_version)`. Falls back to empty on
/// any layout mismatch rather than emitting garbage.
pub(crate) fn resolve_system_properties(path: &str, p1: &Pass1) -> io::Result<SystemProps> {
    let empty = (Vec::new(), None);
    let id_size = p1.id_size;
    let obj_ref_width = id_size as usize;
    let class_map = &p1.class_map;
    let strings = &p1.strings;

    // ── P0: locate java/lang/System's static `props` object address ──────────
    let mut props_addr: u64 = 0;
    scan_class_dumps(path, id_size, |class_obj_id, statics| {
        if props_addr != 0 {
            return;
        }
        let cname = class_map
            .get(&class_obj_id)
            .and_then(|ci| strings.get(&ci.name_id))
            .map(|s| s.as_str())
            .unwrap_or("");
        if cname != "java/lang/System" {
            return;
        }
        for &(name_id, type_code, value) in statics {
            if HprofType::from_code(type_code) != Some(HprofType::Object) {
                continue;
            }
            let fname = strings.get(&name_id).map(|s| s.as_str()).unwrap_or("");
            if fname == "props" && value != 0 {
                props_addr = value;
            }
        }
    })?;
    if props_addr == 0 {
        return Ok(empty);
    }

    // ── Round 1: props instance + its `table` Object[] in one pass ───────────
    let wanted_inst_r1: std::collections::HashSet<u64> = std::iter::once(props_addr).collect();
    // We don't know table_addr yet, so collect props instance blob here; we'll
    // derive table_addr in-memory, then collect the obj-array in a second targeted
    // collect_blobs or by reading it from the same pass if we had it up-front.
    // Since props → table is a forward ref with unknown addr, we need the instance
    // first. We collect props instance + (speculatively empty) obj sets here.
    let (inst_blobs_r1, _, _) = collect_blobs(
        path,
        id_size,
        &wanted_inst_r1,
        &std::collections::HashSet::new(),
        &std::collections::HashSet::new(),
    )?;

    let mut table_addr: u64 = 0;
    if let Some(&(class_id, ref blob)) = inst_blobs_r1.get(&props_addr) {
        let off = match field_offset(
            class_id,
            "table",
            "java/util/Hashtable",
            class_map,
            strings,
            obj_ref_width,
        ) {
            Some((o, HprofType::Object)) => o as usize,
            _ => 0,
        };
        if off != 0 && off + obj_ref_width <= blob.len() {
            table_addr = read_ref(&blob[off..], obj_ref_width);
        }
    }
    if table_addr == 0 {
        // No Hashtable `table` field (e.g. Java 9+ ConcurrentHashMap-backed
        // Properties). Fall back gracefully.
        return Ok(empty);
    }

    // ── Round 2a: `table` Object[] → non-null entry slot addresses ───────────
    let wanted_obj_r2: std::collections::HashSet<u64> = std::iter::once(table_addr).collect();
    let (_, _, obj_blobs_r2) = collect_blobs(
        path,
        id_size,
        &std::collections::HashSet::new(),
        &std::collections::HashSet::new(),
        &wanted_obj_r2,
    )?;

    let mut entry_addrs: Vec<u64> = Vec::new();
    if let Some(elem_bytes) = obj_blobs_r2.get(&table_addr) {
        for chunk in elem_bytes.chunks_exact(obj_ref_width) {
            if entry_addrs.len() >= MAX_PROP_ENTRIES {
                break;
            }
            let r = read_ref(chunk, obj_ref_width);
            if r != 0 {
                entry_addrs.push(r);
            }
        }
    }
    if entry_addrs.is_empty() {
        return Ok(empty);
    }

    // ── Round 2b: ALL entry instances + ALL String instances + ALL prim-arrays
    //    in ONE pass. ─────────────────────────────────────────────────────────
    // We collect entry blobs, then follow `next` chains in-memory. Since all
    // entry blobs are in-memory after this pass, no iterative file scans needed.
    // String blobs and backing arrays are also collected here using the same pass,
    // but we don't yet know which Strings will be referenced — we over-collect
    // entry blobs first (all initial entries from the table), decode their
    // key/value/next in-memory, and THEN collect Strings + arrays in Round 3.
    //
    // Strategy: Round 2b = collect initial entry blobs only.
    //           Round 3   = collect all String blobs + backing arrays in one pass.
    let wanted_entries: std::collections::HashSet<u64> = entry_addrs.iter().copied().collect();
    let (entry_blobs, _, _) = collect_blobs(
        path,
        id_size,
        &wanted_entries,
        &std::collections::HashSet::new(),
        &std::collections::HashSet::new(),
    )?;

    // Follow chains in-memory, collecting newly discovered `next` addrs.
    // We may need more entry blobs for chain tails not in the initial table.
    // Find which next-addrs aren't yet collected, then do one more targeted pass.
    let mut all_entry_blobs = entry_blobs;
    let mut entry_off_cache: HashMap<u64, Option<(usize, usize, usize)>> = HashMap::new();

    // First pass over initial entries to find chained addrs not yet fetched.
    let mut chained_addrs: std::collections::HashSet<u64> = std::collections::HashSet::new();
    for (&_addr, &(class_id, ref blob)) in &all_entry_blobs {
        let offs = entry_off_cache.entry(class_id).or_insert_with(|| {
            let key_off = match field_offset(class_id, "key", "java/util/Hashtable$Entry", class_map, strings, obj_ref_width) {
                Some((o, HprofType::Object)) => o as usize,
                _ => return None,
            };
            let value_off = match field_offset(class_id, "value", "java/util/Hashtable$Entry", class_map, strings, obj_ref_width) {
                Some((o, HprofType::Object)) => o as usize,
                _ => return None,
            };
            let next_off = match field_offset(class_id, "next", "java/util/Hashtable$Entry", class_map, strings, obj_ref_width) {
                Some((o, HprofType::Object)) => o as usize,
                _ => return None,
            };
            Some((key_off, value_off, next_off))
        });
        if let Some((_, _, next_off)) = *offs {
            if next_off + obj_ref_width <= blob.len() {
                let next_ref = read_ref(&blob[next_off..], obj_ref_width);
                if next_ref != 0 && !all_entry_blobs.contains_key(&next_ref) {
                    chained_addrs.insert(next_ref);
                }
            }
        }
    }

    // Iteratively collect chain tails (bounded by MAX_PROP_ENTRIES).
    let mut depth = 0u32;
    while !chained_addrs.is_empty() && all_entry_blobs.len() < MAX_PROP_ENTRIES && depth < 64 {
        depth += 1;
        let (more_blobs, _, _) = collect_blobs(
            path,
            id_size,
            &chained_addrs,
            &std::collections::HashSet::new(),
            &std::collections::HashSet::new(),
        )?;
        chained_addrs.clear();
        for (&_addr, &(class_id, ref blob)) in &more_blobs {
            let offs = entry_off_cache.entry(class_id).or_insert_with(|| {
                let key_off = match field_offset(class_id, "key", "java/util/Hashtable$Entry", class_map, strings, obj_ref_width) {
                    Some((o, HprofType::Object)) => o as usize,
                    _ => return None,
                };
                let value_off = match field_offset(class_id, "value", "java/util/Hashtable$Entry", class_map, strings, obj_ref_width) {
                    Some((o, HprofType::Object)) => o as usize,
                    _ => return None,
                };
                let next_off = match field_offset(class_id, "next", "java/util/Hashtable$Entry", class_map, strings, obj_ref_width) {
                    Some((o, HprofType::Object)) => o as usize,
                    _ => return None,
                };
                Some((key_off, value_off, next_off))
            });
            if let Some((_, _, next_off)) = *offs {
                if next_off + obj_ref_width <= blob.len() {
                    let next_ref = read_ref(&blob[next_off..], obj_ref_width);
                    if next_ref != 0
                        && !all_entry_blobs.contains_key(&next_ref)
                        && all_entry_blobs.len() + chained_addrs.len() < MAX_PROP_ENTRIES
                    {
                        chained_addrs.insert(next_ref);
                    }
                }
            }
        }
        all_entry_blobs.extend(more_blobs);
    }

    // Decode all collected entry blobs → key/value String addrs.
    let mut key_val: HashMap<u64, (u64, u64)> = HashMap::new();
    for (&addr, &(class_id, ref blob)) in &all_entry_blobs {
        let Some(&Some((key_off, value_off, _next_off))) = entry_off_cache.get(&class_id) else {
            continue;
        };
        if key_off + obj_ref_width > blob.len() || value_off + obj_ref_width > blob.len() {
            continue;
        }
        let key_ref = read_ref(&blob[key_off..], obj_ref_width);
        let value_ref = read_ref(&blob[value_off..], obj_ref_width);
        key_val.insert(addr, (key_ref, value_ref));
    }
    drop(all_entry_blobs);
    if key_val.is_empty() {
        return Ok(empty);
    }

    // ── Round 3: String instances + backing PRIM_ARRAYs in ONE pass ──────────
    let mut wanted_strings: std::collections::HashSet<u64> = std::collections::HashSet::new();
    for &(k, v) in key_val.values() {
        if k != 0 { wanted_strings.insert(k); }
        if v != 0 { wanted_strings.insert(v); }
    }

    // Collect String blobs to derive array addrs, then collect arrays in same call
    // after we know them. Since we need String blobs first to get array addrs,
    // we must do 2 sub-steps — but we can fuse them into one collect_blobs call
    // only if we also pass wanted_prim. We don't know wanted_prim yet.
    // Solution: collect String blobs first, then collect arrays.
    let (str_blobs, _, _) = collect_blobs(
        path,
        id_size,
        &wanted_strings,
        &std::collections::HashSet::new(),
        &std::collections::HashSet::new(),
    )?;

    let mut string_to_arr: HashMap<u64, (u64, u8)> = HashMap::new();
    let mut str_off_cache: HashMap<u64, Option<(usize, Option<usize>)>> = HashMap::new();
    for (&addr, &(class_id, ref blob)) in &str_blobs {
        let offs = *str_off_cache.entry(class_id).or_insert_with(|| {
            let value_off = match field_offset(class_id, "value", "java/lang/String", class_map, strings, obj_ref_width) {
                Some((off, HprofType::Object)) => off as usize,
                _ => return None,
            };
            let coder_off = match field_offset(class_id, "coder", "java/lang/String", class_map, strings, obj_ref_width) {
                Some((off, HprofType::Byte)) => Some(off as usize),
                _ => None,
            };
            Some((value_off, coder_off))
        });
        if let Some((value_off, coder_off)) = offs {
            if value_off + obj_ref_width <= blob.len() {
                let arr_ref = read_ref(&blob[value_off..], obj_ref_width);
                let coder = match coder_off {
                    Some(co) if co < blob.len() => blob[co],
                    _ => 1,
                };
                if arr_ref != 0 {
                    string_to_arr.insert(addr, (arr_ref, coder));
                }
            }
        }
    }
    drop(str_blobs);

    let wanted_arrays: std::collections::HashSet<u64> =
        string_to_arr.values().map(|&(a, _)| a).collect();
    let (_, arr_blobs, _) = collect_blobs(
        path,
        id_size,
        &std::collections::HashSet::new(),
        &wanted_arrays,
        &std::collections::HashSet::new(),
    )?;

    // ── Decode ────────────────────────────────────────────────────────────────
    let decode = |str_addr: u64| -> Option<String> {
        if str_addr == 0 { return None; }
        let &(arr_addr, coder) = string_to_arr.get(&str_addr)?;
        let bytes = arr_blobs.get(&arr_addr)?;
        Some(decode_java_string(bytes, coder))
    };
    let mut pairs: Vec<(String, String)> = Vec::new();
    for &(k, v) in key_val.values() {
        let (Some(key), Some(value)) = (decode(k), decode(v)) else { continue; };
        if key.is_empty() { continue; }
        pairs.push((key, value));
    }
    pairs.sort();
    pairs.dedup();

    let find = |key: &str| -> Option<String> {
        pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone())
    };
    let jvm_version = find("java.vm.version").or_else(|| find("java.version"));

    Ok((pairs, jvm_version))
}

/// Render one stack frame as `class.method (source:line)`, applying HPROF's
/// line-number conventions (>0 = line; -1 unknown; -2 compiled; -3 native).
/// Missing strings fall back to placeholders so a frame is always printable.
pub(crate) fn render_frame(
    class: Option<&str>,
    method: Option<&str>,
    source: Option<&str>,
    class_serial: u32,
    line_number: i32,
) -> String {
    let class = class
        .map(|c| c.to_string())
        .unwrap_or_else(|| format!("<class#{class_serial}>"));
    let method = method.unwrap_or("<method>");
    let source = source.unwrap_or("Unknown Source");
    let loc = match line_number {
        n if n > 0 => format!("{source}:{n}"),
        -2 => format!("{source}(Compiled Method)"),
        -3 => "Native Method".to_string(),
        _ => source.to_string(),
    };
    format!("{class}.{method} ({loc})")
}

/// Convert an internal binary class name (`Lfoo/Bar;` or `foo/Bar`) into the
/// dotted display form used in stack frames (`foo.Bar`).
pub(crate) fn pretty_binary_name(name: &str) -> String {
    let trimmed = name.strip_prefix('L').unwrap_or(name);
    let trimmed = trimmed.strip_suffix(';').unwrap_or(trimmed);
    trimmed.replace('/', ".")
}

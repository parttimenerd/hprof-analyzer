//! Pass-2 thread / alloc-site / system-property resolution + frame formatting.

use std::collections::HashMap;

use std::io;

use crate::{pass1::Pass1, types::HprofType};

use super::{
    ThreadProps, ThreadStack, decode_java_string, field_offset, read_ref, scan_class_dumps,
    scan_instance_blobs, scan_obj_arrays, scan_prim_arrays,
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
/// Runs THREE extra full-file sequential scans (the reader is streaming-only, so
/// each multi-hop forward reference needs its own pass over the file):
///   A: Thread object → its `name` String address (and the scalar props inline).
///   B: String → its backing array address + `coder` byte (Java 8 has no coder).
///   C: backing PRIM_ARRAY → its raw element bytes.
/// Then chains the maps per serial and decodes. Passes are NOT merged because a
/// hop's target addresses are only known after the previous pass completes. The
/// scalar props add zero extra passes — they piggyback the Pass-A blob read.
///
/// Field offsets are derived from each object's ACTUAL class id (memoized),
/// because a heap may hold several loader-distinct class objects named
/// `java/lang/Thread` / `java/lang/String`, and thread objects are frequently
/// subclasses whose inherited `name` sits past the subclass's own fields.
pub(crate) fn resolve_thread_names(
    path: &str,
    p1: &Pass1,
) -> io::Result<HashMap<u32, ThreadProps>> {
    let mut props: HashMap<u32, ThreadProps> = HashMap::new();
    if p1.thread_serial_to_obj_id.is_empty() {
        return Ok(props);
    }
    let id_size = p1.id_size;
    // Object references inside an INSTANCE_DUMP blob are always id_size wide (the
    // compressed-oops narrowing detected for array elements does not apply here).
    let obj_ref_width = id_size as usize;
    let class_map = &p1.class_map;
    let strings = &p1.strings;

    // ── Pass A: Thread object addr → name String addr + always-on scalars ─────
    // Bounded by #threads. Offsets are resolved from each thread's own class id
    // (memoized), matching only fields declared by java/lang/Thread so a subclass
    // field of the same simple name cannot shadow them. The name is followed via
    // Passes B/C below; daemon/priority/threadStatus are read from this blob when
    // Thread declares them directly (JDK 8-16), else from the `holder`
    // (Thread$FieldHolder) object via one extra bounded pass (JDK 17+).
    // contextClassLoader stays on Thread in every layout.
    let wanted_threads: std::collections::HashSet<u64> =
        p1.thread_serial_to_obj_id.values().copied().collect();
    let mut thread_to_name_addr: HashMap<u64, u64> = HashMap::new();
    // thread obj addr → (is_daemon, priority, thread_status, context_loader_addr)
    let mut thread_to_scalars: HashMap<u64, (bool, i32, i32, u64)> = HashMap::new();
    // thread obj addr → holder object addr (JDK 17+ FieldHolder layout; else 0).
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
    let read_i32 = |blob: &[u8], o: usize| -> Option<i32> {
        blob.get(o..o + 4)
            .map(|b| i32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    };
    scan_instance_blobs(path, id_size, &wanted_threads, |addr, class_id, blob| {
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
            // JDK 17+ moved daemon/priority/threadStatus into a nested
            // Thread$FieldHolder referenced by `holder`. Capture its offset so a
            // follow-up pass can read the scalars from there.
            let holder_off = obj_off("holder");
            (
                name_off,
                daemon_off,
                priority_off,
                status_off,
                ctx_off,
                holder_off,
            )
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
        let is_daemon = daemon_off
            .and_then(|o| blob.get(o))
            .map(|&b| b != 0)
            .unwrap_or(false);
        let priority = priority_off.and_then(|o| read_i32(blob, o)).unwrap_or(0);
        let thread_status = status_off.and_then(|o| read_i32(blob, o)).unwrap_or(0);
        let context_loader_addr = ctx_off
            .filter(|&o| o + obj_ref_width <= blob.len())
            .map(|o| read_ref(&blob[o..], obj_ref_width))
            .unwrap_or(0);
        thread_to_scalars.insert(
            addr,
            (is_daemon, priority, thread_status, context_loader_addr),
        );
        // Record the holder addr only when the scalars are NOT directly on Thread
        // (i.e. the FieldHolder layout), so the extra pass is skipped for JDK 8-16.
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
    })?;

    // ── Pass A2: Thread$FieldHolder → daemon/priority/threadStatus (JDK 17+) ──
    // Only runs when the FieldHolder layout was detected (thread_to_holder
    // non-empty). Bounded by #threads. Reads the three scalars from each holder
    // blob and folds them back into thread_to_scalars.
    if !thread_to_holder.is_empty() {
        let wanted_holders: std::collections::HashSet<u64> =
            thread_to_holder.values().copied().collect();
        // holder_addr → (daemon, priority, threadStatus)
        let mut holder_scalars: HashMap<u64, (bool, i32, i32)> = HashMap::new();
        // class_id → (daemon_off, priority_off, status_off)
        let mut holder_off_cache: HashMap<u64, HolderOffsets> = HashMap::new();
        scan_instance_blobs(path, id_size, &wanted_holders, |addr, class_id, blob| {
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
            let is_daemon = daemon_off
                .and_then(|o| blob.get(o))
                .map(|&b| b != 0)
                .unwrap_or(false);
            let priority = priority_off.and_then(|o| read_i32(blob, o)).unwrap_or(0);
            let thread_status = status_off.and_then(|o| read_i32(blob, o)).unwrap_or(0);
            holder_scalars.insert(addr, (is_daemon, priority, thread_status));
        })?;
        for (&thread_addr, &holder_addr) in &thread_to_holder {
            if let Some(&(d, p, s)) = holder_scalars.get(&holder_addr) {
                if let Some(entry) = thread_to_scalars.get_mut(&thread_addr) {
                    entry.0 = d;
                    entry.1 = p;
                    entry.2 = s;
                }
            }
        }
    }

    // Seed the props map with the scalar overview fields for every resolvable
    // thread (name filled in after Passes B/C). Threads with no INSTANCE_DUMP
    // blob simply get defaults.
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
    if thread_to_name_addr.is_empty() {
        return Ok(props);
    }

    // ── Pass B: String addr → (array addr, coder) ────────────────────────────
    // Bounded by #threads (one name String each). The value/coder offsets are
    // resolved per String's own class id (memoized): Java 8 char[] Strings have
    // no `coder` field and are treated as UTF16 (coder 1).
    let wanted_strings: std::collections::HashSet<u64> =
        thread_to_name_addr.values().copied().collect();
    let mut string_to_arr: HashMap<u64, (u64, u8)> = HashMap::new();
    // class_id → (value_off, coder_off)
    let mut str_off_cache: HashMap<u64, Option<(usize, Option<usize>)>> = HashMap::new();
    scan_instance_blobs(path, id_size, &wanted_strings, |addr, class_id, blob| {
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
                // Java 8 char[]: no coder field → UTF16 (coder 1).
                let coder = match coder_off {
                    Some(co) if co < blob.len() => blob[co],
                    _ => 1,
                };
                if arr_ref != 0 {
                    string_to_arr.insert(addr, (arr_ref, coder));
                }
            }
        }
    })?;
    if string_to_arr.is_empty() {
        return Ok(props);
    }

    // ── Pass C: array addr → element bytes ───────────────────────────────────
    // Bounded by #threads (each name array is tiny).
    let wanted_arrays: std::collections::HashSet<u64> =
        string_to_arr.values().map(|&(a, _)| a).collect();
    let mut arr_bytes: HashMap<u64, Vec<u8>> = HashMap::new();
    scan_prim_arrays(path, id_size, &wanted_arrays, |addr, bytes| {
        arr_bytes.insert(addr, bytes.to_vec());
    })?;

    // ── Decode: chain serial → thread → String → array → text ────────────────
    for (&serial, &thread_addr) in &p1.thread_serial_to_obj_id {
        let Some(&name_addr) = thread_to_name_addr.get(&thread_addr) else {
            continue;
        };
        let Some(&(arr_addr, coder)) = string_to_arr.get(&name_addr) else {
            continue;
        };
        let Some(bytes) = arr_bytes.get(&arr_addr) else {
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
///   P1: props object → its `table` Object[] array address (Properties extends
///       Hashtable; `table` is declared by java/util/Hashtable). Java 9+
///       Properties that delegate to a ConcurrentHashMap have no such field →
///       graceful empty fallback.
///   P2: `table` Object[] → the non-null Hashtable$Entry slot addresses.
///   P3: entries → (key,value,next) refs; follow `next` chains (bounded by the
///       4096 cap) to collect all key/value String addresses.
///   P4: key/value Strings → (backing array addr, coder).
///   P5: backing PRIM_ARRAYs → raw bytes → decode.
///
/// Returns `(sorted (key,value) pairs, jvm_version)`. The JVM version is derived
/// even when the property table itself is empty (both keys come from the pairs).
/// On ANY layout mismatch the property list falls back to empty rather than
/// emitting garbage.
pub(crate) fn resolve_system_properties(path: &str, p1: &Pass1) -> io::Result<SystemProps> {
    let empty = (Vec::new(), None);
    let id_size = p1.id_size;
    let obj_ref_width = id_size as usize;
    let class_map = &p1.class_map;
    let strings = &p1.strings;

    // ── P0: locate java/lang/System's static `props` object address ──────────
    // Bounded: ONE class' static fields. Scan CLASS_DUMP records; for the class
    // whose name is "java/lang/System", read the OBJECT static field "props".
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

    // ── P1: props object → its Hashtable `table` Object[] array address ───────
    // Bounded: ONE object. `table` is declared by java/util/Hashtable; matching
    // that owner avoids a subclass field of the same simple name shadowing it.
    let mut table_addr: u64 = 0;
    let wanted_props: std::collections::HashSet<u64> = std::iter::once(props_addr).collect();
    scan_instance_blobs(path, id_size, &wanted_props, |_addr, class_id, blob| {
        let off = match field_offset(
            class_id,
            "table",
            "java/util/Hashtable",
            class_map,
            strings,
            obj_ref_width,
        ) {
            Some((o, HprofType::Object)) => o as usize,
            _ => return,
        };
        if off + obj_ref_width <= blob.len() {
            table_addr = read_ref(&blob[off..], obj_ref_width);
        }
    })?;
    if table_addr == 0 {
        // No Hashtable `table` field (e.g. Java 9+ ConcurrentHashMap-backed
        // Properties). Fall back gracefully — no properties, no jvm_version.
        return Ok(empty);
    }

    // ── P2: `table` Object[] → non-null Hashtable$Entry slot addresses ────────
    // Bounded to MAX_PROP_ENTRIES.
    let wanted_table: std::collections::HashSet<u64> = std::iter::once(table_addr).collect();
    let mut entry_addrs: Vec<u64> = Vec::new();
    scan_obj_arrays(path, id_size, &wanted_table, |_addr, elem_refs| {
        for chunk in elem_refs.chunks_exact(obj_ref_width) {
            if entry_addrs.len() >= MAX_PROP_ENTRIES {
                break;
            }
            let r = read_ref(chunk, obj_ref_width);
            if r != 0 {
                entry_addrs.push(r);
            }
        }
    })?;
    if entry_addrs.is_empty() {
        return Ok(empty);
    }

    // ── P3: entries → (key,value,next) refs; follow `next` chains ─────────────
    // Bounded to MAX_PROP_ENTRIES entries total. Chains can add more entry
    // addresses to resolve, so iterate the worklist across repeated bounded
    // scans until it stabilizes (chains are short; capped by the entry budget).
    // key_val: entry addr → (key String addr, value String addr).
    let mut key_val: HashMap<u64, (u64, u64)> = HashMap::new();
    let mut pending: std::collections::HashSet<u64> = entry_addrs.iter().copied().collect();
    let mut entry_off_cache: HashMap<u64, Option<(usize, usize, usize)>> = HashMap::new();
    // Bound the number of chain-following passes; each pass resolves at least
    // one hop of every chain, so 64 caps the deepest Hashtable bucket chain we
    // will follow (real buckets are 1-3 deep). Combined with the entry budget
    // this is the fixed worst-case extra-pass ceiling; it terminates well before
    // it in practice.
    for _ in 0..64 {
        if pending.is_empty() || key_val.len() >= MAX_PROP_ENTRIES {
            break;
        }
        let mut next_pending: std::collections::HashSet<u64> = std::collections::HashSet::new();
        scan_instance_blobs(path, id_size, &pending, |addr, class_id, blob| {
            if key_val.contains_key(&addr) {
                return;
            }
            let offs = *entry_off_cache.entry(class_id).or_insert_with(|| {
                let key_off = match field_offset(
                    class_id,
                    "key",
                    "java/util/Hashtable$Entry",
                    class_map,
                    strings,
                    obj_ref_width,
                ) {
                    Some((o, HprofType::Object)) => o as usize,
                    _ => return None,
                };
                let value_off = match field_offset(
                    class_id,
                    "value",
                    "java/util/Hashtable$Entry",
                    class_map,
                    strings,
                    obj_ref_width,
                ) {
                    Some((o, HprofType::Object)) => o as usize,
                    _ => return None,
                };
                let next_off = match field_offset(
                    class_id,
                    "next",
                    "java/util/Hashtable$Entry",
                    class_map,
                    strings,
                    obj_ref_width,
                ) {
                    Some((o, HprofType::Object)) => o as usize,
                    _ => return None,
                };
                Some((key_off, value_off, next_off))
            });
            let Some((key_off, value_off, next_off)) = offs else {
                return;
            };
            if key_off + obj_ref_width > blob.len()
                || value_off + obj_ref_width > blob.len()
                || next_off + obj_ref_width > blob.len()
            {
                return;
            }
            let key_ref = read_ref(&blob[key_off..], obj_ref_width);
            let value_ref = read_ref(&blob[value_off..], obj_ref_width);
            let next_ref = read_ref(&blob[next_off..], obj_ref_width);
            key_val.insert(addr, (key_ref, value_ref));
            if next_ref != 0
                && !key_val.contains_key(&next_ref)
                && key_val.len() + next_pending.len() < MAX_PROP_ENTRIES
            {
                next_pending.insert(next_ref);
            }
        })?;
        pending = next_pending;
    }
    if key_val.is_empty() {
        return Ok(empty);
    }

    // ── P4: key/value Strings → (backing array addr, coder) ───────────────────
    // Bounded by 2 * #entries. Reuses the Stage-2 String decode field offsets.
    let mut wanted_strings: std::collections::HashSet<u64> = std::collections::HashSet::new();
    for &(k, v) in key_val.values() {
        if k != 0 {
            wanted_strings.insert(k);
        }
        if v != 0 {
            wanted_strings.insert(v);
        }
    }
    let mut string_to_arr: HashMap<u64, (u64, u8)> = HashMap::new();
    let mut str_off_cache: HashMap<u64, Option<(usize, Option<usize>)>> = HashMap::new();
    scan_instance_blobs(path, id_size, &wanted_strings, |addr, class_id, blob| {
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
                    _ => 1,
                };
                if arr_ref != 0 {
                    string_to_arr.insert(addr, (arr_ref, coder));
                }
            }
        }
    })?;

    // ── P5: backing PRIM_ARRAYs → raw bytes ───────────────────────────────────
    // Bounded by the number of distinct backing arrays (≤ 2 * #entries).
    let wanted_arrays: std::collections::HashSet<u64> =
        string_to_arr.values().map(|&(a, _)| a).collect();
    let mut arr_bytes: HashMap<u64, Vec<u8>> = HashMap::new();
    scan_prim_arrays(path, id_size, &wanted_arrays, |addr, bytes| {
        arr_bytes.insert(addr, bytes.to_vec());
    })?;

    // ── Decode: entry → key text, value text ─────────────────────────────────
    let decode = |str_addr: u64| -> Option<String> {
        if str_addr == 0 {
            return None;
        }
        let &(arr_addr, coder) = string_to_arr.get(&str_addr)?;
        let bytes = arr_bytes.get(&arr_addr)?;
        Some(decode_java_string(bytes, coder))
    };
    let mut pairs: Vec<(String, String)> = Vec::new();
    for &(k, v) in key_val.values() {
        let (Some(key), Some(value)) = (decode(k), decode(v)) else {
            continue;
        };
        if key.is_empty() {
            continue;
        }
        pairs.push((key, value));
    }
    // Deterministic: sort by key (then value), dedup exact duplicates.
    pairs.sort();
    pairs.dedup();

    // ── Derive JVM version: prefer java.vm.version, else java.version ─────────
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

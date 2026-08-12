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

/// Blob-map aliases used by the shared-scan worklist driver. Mirror the three
/// return maps of [`collect_blobs`]: instance blobs (addr → (class_id, bytes)),
/// primitive-array blobs (addr → bytes), object-array blobs (addr → bytes).
type InstMap = HashMap<u64, (u64, Vec<u8>)>;
type PrimMap = HashMap<u64, Vec<u8>>;
type ObjMap = HashMap<u64, Vec<u8>>;
/// A per-round set of wanted addresses: (instance, prim-array, object-array).
type Wants = (
    std::collections::HashSet<u64>,
    std::collections::HashSet<u64>,
    std::collections::HashSet<u64>,
);

/// A bounded object-subgraph resolver that proceeds in rounds. Each round it
/// declares the addresses it needs ([`next_wants`]), the driver fetches them in
/// ONE shared file scan alongside other worklists, then hands the blobs back
/// ([`ingest`]). Returning an all-empty [`Wants`] means the worklist is finished.
///
/// This lets independent resolvers (thread names + system properties) share
/// physical 34 GB scans instead of each paying its own sequence of full-file
/// passes. The decode logic inside each implementor is byte-for-byte identical
/// to the old standalone resolvers — only *when* bytes are read changed.
trait BlobWorklist {
    fn next_wants(&mut self) -> Wants;
    fn ingest(&mut self, inst: &InstMap, prim: &PrimMap, obj: &ObjMap);
}

/// Drive a set of [`BlobWorklist`]s to completion, unioning their per-round
/// wanted sets so each physical file scan serves every still-active worklist.
/// Loops until all worklists report done (all-empty wants). An empty union
/// emits no scan (matching `collect_blobs`' own empty-set fast return).
fn drive_shared_worklists<O>(
    open: O,
    id_size: u8,
    lists: &mut [&mut dyn BlobWorklist],
) -> io::Result<()>
where
    O: Fn() -> io::Result<crate::reader::HprofReader>,
{
    loop {
        let mut wi: std::collections::HashSet<u64> = std::collections::HashSet::new();
        let mut wp: std::collections::HashSet<u64> = std::collections::HashSet::new();
        let mut wo: std::collections::HashSet<u64> = std::collections::HashSet::new();
        for l in lists.iter_mut() {
            let (i, p, o) = l.next_wants();
            wi.extend(i);
            wp.extend(p);
            wo.extend(o);
        }
        if wi.is_empty() && wp.is_empty() && wo.is_empty() {
            break;
        }
        let (inst, prim, obj) = collect_blobs(&open, id_size, &wi, &wp, &wo)?;
        for l in lists.iter_mut() {
            l.ingest(&inst, &prim, &obj);
        }
    }
    Ok(())
}

/// Resolve thread names/scalars AND system properties in one interleaved set of
/// shared file scans. Both subgraphs are independent, so the driver unions their
/// per-round wanted sets and fetches them together — cutting ~8 sequential full
/// scans (the old sum of both resolvers) down toward ~5. Byte-exact output is
/// preserved: each worklist runs the same decode logic as its standalone
/// counterpart. Returns `(thread props, system props, jvm version)`.
pub(crate) fn resolve_thread_and_props<O>(
    open: O,
    p1: &Pass1,
    prefetched_thread_blobs: HashMap<u64, (u64, Vec<u8>)>,
    prefetched_props_addr: u64,
    prefetched_props_blob: Option<(u64, Vec<u8>)>,
) -> io::Result<(HashMap<u32, ThreadProps>, SystemProps)>
where
    O: Fn() -> io::Result<crate::reader::HprofReader>,
{
    let mut threads = ThreadWorklist::new(p1, prefetched_thread_blobs);
    let mut props = PropsWorklist::new(&open, p1, prefetched_props_addr, prefetched_props_blob)?;
    {
        let mut lists: [&mut dyn BlobWorklist; 2] = [&mut threads, &mut props];
        drive_shared_worklists(&open, p1.id_size, &mut lists)?;
    }
    Ok((threads.finish(p1), props.finish()))
}

// class_id → (name_off, daemon_off, priority_off, status_off, ctx_off, holder_off)
type ThreadOffs = (
    Option<usize>,
    Option<usize>,
    Option<usize>,
    Option<usize>,
    Option<usize>,
    Option<usize>,
);

/// [`BlobWorklist`] form of the thread-name/scalar resolver. Rounds:
/// R1 = any thread blobs missing from the 2a prefetch (usually none),
/// R2 = name Strings ∪ FieldHolder objects, R3 = backing prim arrays.
/// Decode logic is identical to the old standalone thread resolver; only scan
/// scheduling is externalized so it can share physical scans with [`PropsWorklist`].
struct ThreadWorklist<'a> {
    p1: &'a Pass1,
    obj_ref_width: usize,
    stage: u8,
    inst_blobs_r1: HashMap<u64, (u64, Vec<u8>)>,
    thread_to_name_addr: HashMap<u64, u64>,
    thread_to_scalars: HashMap<u64, (bool, i32, i32, u64)>,
    thread_to_holder: HashMap<u64, u64>,
    off_cache: HashMap<u64, ThreadOffs>,
    wanted_strings: std::collections::HashSet<u64>,
    wanted_holders: std::collections::HashSet<u64>,
    string_to_arr: HashMap<u64, (u64, u8)>,
    holder_scalars: HashMap<u64, (bool, i32, i32)>,
    arr_blobs: PrimMap,
}

impl<'a> ThreadWorklist<'a> {
    fn new(p1: &'a Pass1, prefetched_thread_blobs: HashMap<u64, (u64, Vec<u8>)>) -> Self {
        ThreadWorklist {
            p1,
            obj_ref_width: p1.id_size as usize,
            // stage 3 = "done, empty" when there are no threads at all.
            stage: if p1.thread_serial_to_obj_id.is_empty() {
                3
            } else {
                0
            },
            inst_blobs_r1: prefetched_thread_blobs,
            thread_to_name_addr: HashMap::new(),
            thread_to_scalars: HashMap::new(),
            thread_to_holder: HashMap::new(),
            off_cache: HashMap::new(),
            wanted_strings: std::collections::HashSet::new(),
            wanted_holders: std::collections::HashSet::new(),
            string_to_arr: HashMap::new(),
            holder_scalars: HashMap::new(),
            arr_blobs: HashMap::new(),
        }
    }

    /// Decode thread R1 blobs (`inst_blobs_r1`) into name-addr / scalar / holder
    /// maps. Verbatim from `resolve_thread_names` R1 body.
    fn decode_thread_blobs(&mut self) {
        let class_map = &self.p1.class_map;
        let strings = &self.p1.strings;
        let obj_ref_width = self.obj_ref_width;
        let read_i32 = |blob: &[u8], o: usize| -> Option<i32> {
            blob.get(o..o + 4)
                .map(|b| i32::from_be_bytes([b[0], b[1], b[2], b[3]]))
        };
        for (&addr, &(class_id, ref blob)) in &self.inst_blobs_r1 {
            let offs = *self.off_cache.entry(class_id).or_insert_with(|| {
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
                        self.thread_to_name_addr.insert(addr, name_ref);
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
            self.thread_to_scalars.insert(
                addr,
                (is_daemon, priority, thread_status, context_loader_addr),
            );
            if priority_off.is_none() && daemon_off.is_none() && status_off.is_none() {
                if let Some(off) = holder_off {
                    if off + obj_ref_width <= blob.len() {
                        let href = read_ref(&blob[off..], obj_ref_width);
                        if href != 0 {
                            self.thread_to_holder.insert(addr, href);
                        }
                    }
                }
            }
        }
        self.inst_blobs_r1 = HashMap::new();
    }

    fn finish(mut self, p1: &Pass1) -> HashMap<u32, ThreadProps> {
        let mut props: HashMap<u32, ThreadProps> = HashMap::new();
        // Fold holder scalars back into thread_to_scalars.
        for (&thread_addr, &holder_addr) in &self.thread_to_holder {
            if let Some(&(d, p, s)) = self.holder_scalars.get(&holder_addr) {
                if let Some(entry) = self.thread_to_scalars.get_mut(&thread_addr) {
                    entry.0 = d;
                    entry.1 = p;
                    entry.2 = s;
                }
            }
        }
        // Seed props with scalar overview fields.
        for (&serial, &thread_addr) in &p1.thread_serial_to_obj_id {
            if let Some(&(is_daemon, priority, thread_status, ctx)) =
                self.thread_to_scalars.get(&thread_addr)
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
        // Decode: serial → thread → String → array → text.
        for (&serial, &thread_addr) in &p1.thread_serial_to_obj_id {
            let Some(&name_addr) = self.thread_to_name_addr.get(&thread_addr) else {
                continue;
            };
            let Some(&(arr_addr, coder)) = self.string_to_arr.get(&name_addr) else {
                continue;
            };
            let Some(bytes) = self.arr_blobs.get(&arr_addr) else {
                continue;
            };
            let text = decode_java_string(bytes, coder);
            if !text.is_empty() {
                props.entry(serial).or_default().name = text;
            }
        }
        props
    }
}

impl<'a> BlobWorklist for ThreadWorklist<'a> {
    fn next_wants(&mut self) -> Wants {
        let empty = || {
            (
                std::collections::HashSet::new(),
                std::collections::HashSet::new(),
                std::collections::HashSet::new(),
            )
        };
        match self.stage {
            0 => {
                // R1: any thread blobs not already prefetched.
                let missing: std::collections::HashSet<u64> = self
                    .p1
                    .thread_serial_to_obj_id
                    .values()
                    .copied()
                    .filter(|a| !self.inst_blobs_r1.contains_key(a))
                    .collect();
                self.stage = 1;
                (
                    missing,
                    std::collections::HashSet::new(),
                    std::collections::HashSet::new(),
                )
            }
            1 => {
                // R1 blobs are in; decode them, then request R2 (strings ∪ holders).
                self.decode_thread_blobs();
                self.wanted_strings = self.thread_to_name_addr.values().copied().collect();
                self.wanted_holders = self.thread_to_holder.values().copied().collect();
                let inst: std::collections::HashSet<u64> = self
                    .wanted_strings
                    .iter()
                    .chain(self.wanted_holders.iter())
                    .copied()
                    .collect();
                self.stage = 2;
                if inst.is_empty() {
                    // No strings/holders → nothing more to fetch; skip to done.
                    self.stage = 3;
                    empty()
                } else {
                    (
                        inst,
                        std::collections::HashSet::new(),
                        std::collections::HashSet::new(),
                    )
                }
            }
            2 => {
                // R2 blobs are in (ingested); request R3 (backing prim arrays).
                let arrays: std::collections::HashSet<u64> =
                    self.string_to_arr.values().map(|&(a, _)| a).collect();
                self.stage = 3;
                if arrays.is_empty() {
                    empty()
                } else {
                    (
                        std::collections::HashSet::new(),
                        arrays,
                        std::collections::HashSet::new(),
                    )
                }
            }
            _ => empty(),
        }
    }

    fn ingest(&mut self, inst: &InstMap, prim: &PrimMap, _obj: &ObjMap) {
        match self.stage {
            // stage advanced to 1 after emitting R1 wants: take R1 blobs.
            1 => {
                for (&addr, v) in inst {
                    self.inst_blobs_r1.entry(addr).or_insert_with(|| v.clone());
                }
            }
            // stage advanced to 2 after emitting R2 wants: decode strings + holders.
            2 => self.ingest_r2(inst),
            // stage advanced to 3 after emitting R3 wants: take array blobs.
            3 => {
                for (&addr, bytes) in prim {
                    self.arr_blobs.entry(addr).or_insert_with(|| bytes.clone());
                }
            }
            _ => {}
        }
    }
}

impl<'a> ThreadWorklist<'a> {
    /// Decode R2 String + FieldHolder blobs into `string_to_arr` / `holder_scalars`.
    /// Verbatim from `resolve_thread_names` R2 body.
    fn ingest_r2(&mut self, inst_blobs_r2: &InstMap) {
        let class_map = &self.p1.class_map;
        let strings = &self.p1.strings;
        let obj_ref_width = self.obj_ref_width;
        let read_i32 = |blob: &[u8], o: usize| -> Option<i32> {
            blob.get(o..o + 4)
                .map(|b| i32::from_be_bytes([b[0], b[1], b[2], b[3]]))
        };
        let mut str_off_cache: HashMap<u64, Option<(usize, Option<usize>)>> = HashMap::new();
        let mut holder_off_cache: HashMap<u64, HolderOffsets> = HashMap::new();
        for (&addr, &(class_id, ref blob)) in inst_blobs_r2 {
            if self.wanted_strings.contains(&addr) {
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
                            self.string_to_arr.insert(addr, (arr_ref, coder));
                        }
                    }
                }
            } else if self.wanted_holders.contains(&addr) {
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
                self.holder_scalars
                    .insert(addr, (is_daemon, priority, thread_status));
            }
        }
    }
}

/// Maximum number of system-property entries captured. The props table is ONE
/// object, but its slot count is attacker/dump-controlled, so every worklist
/// derived from it is capped at this bound to keep RSS bounded regardless of
/// dump size.
pub(crate) const MAX_PROP_ENTRIES: usize = 4096;

/// Sorted `(key, value)` system-property pairs plus the derived JVM version.
pub(crate) type SystemProps = (Vec<(String, String)>, Option<String>);

/// Cached `(key_off, value_off, next_off)` for a `Hashtable$Entry` class, or
/// `None` if the layout does not match.
type EntryOffs = Option<(usize, usize, usize)>;

/// [`BlobWorklist`] form of the system-properties resolver. Rounds:
/// R1 = props instance, R2a = the `table` Object[], R2b = initial entries,
/// chain-tail = follow `next` chains (0..64 rounds, bounded), R3 = key/value
/// Strings, R3b = backing prim arrays. Decode logic is verbatim from the old
/// standalone system-properties resolver; only scan scheduling is externalized
/// so it can share physical scans with [`ThreadWorklist`].
struct PropsWorklist<'a> {
    p1: &'a Pass1,
    obj_ref_width: usize,
    stage: u8,
    props_addr: u64,
    table_addr: u64,
    entry_addrs: Vec<u64>,
    all_entry_blobs: HashMap<u64, (u64, Vec<u8>)>,
    entry_off_cache: HashMap<u64, EntryOffs>,
    chained_addrs: std::collections::HashSet<u64>,
    chain_depth: u32,
    key_val: HashMap<u64, (u64, u64)>,
    wanted_strings: std::collections::HashSet<u64>,
    string_to_arr: HashMap<u64, (u64, u8)>,
    arr_blobs: PrimMap,
    done: bool,
}

impl<'a> PropsWorklist<'a> {
    fn new<O>(
        open: &O,
        p1: &'a Pass1,
        prefetched_props_addr: u64,
        prefetched_props_blob: Option<(u64, Vec<u8>)>,
    ) -> io::Result<Self>
    where
        O: Fn() -> io::Result<crate::reader::HprofReader>,
    {
        let id_size = p1.id_size;
        let class_map = &p1.class_map;
        let strings = &p1.strings;
        // P0: locate java/lang/System's static `props` object address. Uses the
        // prefetched value when available; otherwise a scan_class_dumps pass.
        let props_addr = if prefetched_props_addr != 0 {
            prefetched_props_addr
        } else {
            let mut found: u64 = 0;
            scan_class_dumps(open, id_size, |class_obj_id, statics| {
                if found != 0 {
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
                        found = value;
                    }
                }
            })?;
            found
        };

        // If the props instance blob was opportunistically captured during the 2a
        // scan, derive table_addr immediately and skip stage 0 (the first
        // collect_blobs round that would otherwise fetch just this blob).
        let (stage, table_addr) = match prefetched_props_blob {
            Some((class_id, ref blob)) if props_addr != 0 => {
                let obj_ref_width = id_size as usize;
                let off = field_offset(
                    class_id,
                    "table",
                    "java/util/Hashtable",
                    class_map,
                    strings,
                    obj_ref_width,
                )
                .and_then(|(o, t)| {
                    if t == HprofType::Object {
                        Some(o as usize)
                    } else {
                        None
                    }
                })
                .unwrap_or(0);
                let taddr = if off != 0 && off + obj_ref_width <= blob.len() {
                    read_ref(&blob[off..], obj_ref_width)
                } else {
                    0
                };
                // If we successfully decoded table_addr, start at stage 1
                // (next_wants at stage 1 requests the table Object[]). Stage 0
                // requested the props instance which we already have; skipping it
                // removes one collect_blobs round (~88s on 34GB). If decoding
                // failed (e.g. non-standard Hashtable layout), fall back to stage 0.
                if taddr != 0 {
                    (1u8, taddr)
                } else {
                    (0u8, 0u64)
                }
            }
            _ => (0u8, 0u64),
        };

        Ok(PropsWorklist {
            p1,
            obj_ref_width: id_size as usize,
            stage,
            props_addr,
            table_addr,
            entry_addrs: Vec::new(),
            all_entry_blobs: HashMap::new(),
            entry_off_cache: HashMap::new(),
            chained_addrs: std::collections::HashSet::new(),
            chain_depth: 0,
            key_val: HashMap::new(),
            wanted_strings: std::collections::HashSet::new(),
            string_to_arr: HashMap::new(),
            arr_blobs: HashMap::new(),
            done: props_addr == 0,
        })
    }

    /// Resolve `(key_off, value_off, next_off)` for a Hashtable$Entry class.
    fn entry_offs(&mut self, class_id: u64) -> EntryOffs {
        let class_map = &self.p1.class_map;
        let strings = &self.p1.strings;
        let obj_ref_width = self.obj_ref_width;
        *self.entry_off_cache.entry(class_id).or_insert_with(|| {
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
        })
    }

    /// Scan the given entry blobs for `next`-chain addrs not yet fetched, adding
    /// them to `chained_addrs` (bounded). Verbatim from the original chain walk.
    fn collect_chain_from(&mut self, blobs: &InstMap) {
        let obj_ref_width = self.obj_ref_width;
        let class_ids: Vec<(u64, u64)> = blobs.iter().map(|(&a, &(c, _))| (a, c)).collect();
        for (addr, class_id) in class_ids {
            let offs = self.entry_offs(class_id);
            if let Some((_, _, next_off)) = offs {
                if let Some((_, blob)) = blobs.get(&addr) {
                    if next_off + obj_ref_width <= blob.len() {
                        let next_ref = read_ref(&blob[next_off..], obj_ref_width);
                        if next_ref != 0
                            && !self.all_entry_blobs.contains_key(&next_ref)
                            && self.all_entry_blobs.len() + self.chained_addrs.len()
                                < MAX_PROP_ENTRIES
                        {
                            self.chained_addrs.insert(next_ref);
                        }
                    }
                }
            }
        }
    }

    fn finish(mut self) -> SystemProps {
        let empty = (Vec::new(), None);
        if self.done && self.key_val.is_empty() {
            return empty;
        }
        // Decode collected entry blobs → key/value String addrs.
        let class_ids: Vec<(u64, u64)> = self
            .all_entry_blobs
            .iter()
            .map(|(&a, &(c, _))| (a, c))
            .collect();
        for (addr, class_id) in class_ids {
            let Some((key_off, value_off, _next_off)) = self.entry_offs(class_id) else {
                continue;
            };
            let Some((_, blob)) = self.all_entry_blobs.get(&addr) else {
                continue;
            };
            if key_off + self.obj_ref_width > blob.len()
                || value_off + self.obj_ref_width > blob.len()
            {
                continue;
            }
            let key_ref = read_ref(&blob[key_off..], self.obj_ref_width);
            let value_ref = read_ref(&blob[value_off..], self.obj_ref_width);
            self.key_val.insert(addr, (key_ref, value_ref));
        }
        if self.key_val.is_empty() {
            return empty;
        }
        // Decode: str_addr → (arr_addr, coder) → bytes → text.
        let string_to_arr = &self.string_to_arr;
        let arr_blobs = &self.arr_blobs;
        let decode = |str_addr: u64| -> Option<String> {
            if str_addr == 0 {
                return None;
            }
            let &(arr_addr, coder) = string_to_arr.get(&str_addr)?;
            let bytes = arr_blobs.get(&arr_addr)?;
            Some(decode_java_string(bytes, coder))
        };
        let mut pairs: Vec<(String, String)> = Vec::new();
        for &(k, v) in self.key_val.values() {
            let (Some(key), Some(value)) = (decode(k), decode(v)) else {
                continue;
            };
            if key.is_empty() {
                continue;
            }
            pairs.push((key, value));
        }
        pairs.sort();
        pairs.dedup();
        let find = |key: &str| -> Option<String> {
            pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone())
        };
        let jvm_version = find("java.vm.version").or_else(|| find("java.version"));
        (pairs, jvm_version)
    }
}

impl<'a> BlobWorklist for PropsWorklist<'a> {
    fn next_wants(&mut self) -> Wants {
        let empty = || {
            (
                std::collections::HashSet::new(),
                std::collections::HashSet::new(),
                std::collections::HashSet::new(),
            )
        };
        if self.done {
            return empty();
        }
        let obj_ref_width = self.obj_ref_width;
        match self.stage {
            0 => {
                // R1: props instance.
                self.stage = 1;
                (
                    std::iter::once(self.props_addr).collect(),
                    std::collections::HashSet::new(),
                    std::collections::HashSet::new(),
                )
            }
            1 => {
                // R1 ingested (table_addr derived). R2a: the `table` Object[].
                self.stage = 2;
                if self.table_addr == 0 {
                    self.done = true;
                    empty()
                } else {
                    (
                        std::collections::HashSet::new(),
                        std::collections::HashSet::new(),
                        std::iter::once(self.table_addr).collect(),
                    )
                }
            }
            2 => {
                // R2a ingested (entry_addrs derived). R2b: entry instances.
                self.stage = 3;
                if self.entry_addrs.is_empty() {
                    self.done = true;
                    empty()
                } else {
                    (
                        self.entry_addrs.iter().copied().collect(),
                        std::collections::HashSet::new(),
                        std::collections::HashSet::new(),
                    )
                }
            }
            3 => {
                // R2b (and later chain rounds) ingested. If there are pending
                // chain-tail addrs and we're under bounds, request them; else
                // move to R3 (strings). Preserves the exact guards.
                if !self.chained_addrs.is_empty()
                    && self.all_entry_blobs.len() < MAX_PROP_ENTRIES
                    && self.chain_depth < 64
                {
                    self.chain_depth += 1;
                    let wants = std::mem::take(&mut self.chained_addrs);
                    // stage stays 3: chain loop re-enters until drained.
                    (
                        wants,
                        std::collections::HashSet::new(),
                        std::collections::HashSet::new(),
                    )
                } else {
                    // Build key/value String want set from decoded entries. We
                    // must decode entries → key_val here to know the strings.
                    self.decode_entries_to_key_val();
                    self.stage = 4;
                    if self.key_val.is_empty() {
                        self.done = true;
                        return empty();
                    }
                    let mut ws: std::collections::HashSet<u64> = std::collections::HashSet::new();
                    for &(k, v) in self.key_val.values() {
                        if k != 0 {
                            ws.insert(k);
                        }
                        if v != 0 {
                            ws.insert(v);
                        }
                    }
                    self.wanted_strings = ws.clone();
                    (
                        ws,
                        std::collections::HashSet::new(),
                        std::collections::HashSet::new(),
                    )
                }
            }
            4 => {
                // R3 ingested (string_to_arr derived). R3b: backing prim arrays.
                self.stage = 5;
                let arrays: std::collections::HashSet<u64> =
                    self.string_to_arr.values().map(|&(a, _)| a).collect();
                if arrays.is_empty() {
                    self.done = true;
                    empty()
                } else {
                    let _ = obj_ref_width;
                    (
                        std::collections::HashSet::new(),
                        arrays,
                        std::collections::HashSet::new(),
                    )
                }
            }
            _ => {
                self.done = true;
                empty()
            }
        }
    }

    fn ingest(&mut self, inst: &InstMap, prim: &PrimMap, obj: &ObjMap) {
        if self.done {
            return;
        }
        let obj_ref_width = self.obj_ref_width;
        match self.stage {
            // stage advanced to 1: R1 props instance → derive table_addr.
            1 => {
                if let Some(&(class_id, ref blob)) = inst.get(&self.props_addr) {
                    let off = match field_offset(
                        class_id,
                        "table",
                        "java/util/Hashtable",
                        &self.p1.class_map,
                        &self.p1.strings,
                        obj_ref_width,
                    ) {
                        Some((o, HprofType::Object)) => o as usize,
                        _ => 0,
                    };
                    if off != 0 && off + obj_ref_width <= blob.len() {
                        self.table_addr = read_ref(&blob[off..], obj_ref_width);
                    }
                }
            }
            // stage advanced to 2: R2a table Object[] → entry slot addrs.
            2 => {
                if let Some(elem_bytes) = obj.get(&self.table_addr) {
                    for chunk in elem_bytes.chunks_exact(obj_ref_width) {
                        if self.entry_addrs.len() >= MAX_PROP_ENTRIES {
                            break;
                        }
                        let r = read_ref(chunk, obj_ref_width);
                        if r != 0 {
                            self.entry_addrs.push(r);
                        }
                    }
                }
            }
            // stage still 3: R2b initial entries OR a chain-tail round. Absorb
            // blobs, then scan them for further `next`-chain addrs.
            3 => {
                let mut newly: InstMap = HashMap::new();
                for (&addr, v) in inst {
                    if let std::collections::hash_map::Entry::Vacant(e) =
                        self.all_entry_blobs.entry(addr)
                    {
                        e.insert(v.clone());
                        newly.insert(addr, v.clone());
                    }
                }
                self.collect_chain_from(&newly);
            }
            // stage advanced to 4: R3 Strings → string_to_arr.
            4 => self.ingest_strings(inst),
            // stage advanced to 5: R3b backing prim arrays.
            5 => {
                for (&addr, bytes) in prim {
                    self.arr_blobs.entry(addr).or_insert_with(|| bytes.clone());
                }
            }
            _ => {}
        }
    }
}

impl<'a> PropsWorklist<'a> {
    /// Decode all collected entry blobs into `key_val` (addr → (key_ref, value_ref)).
    /// Verbatim from the original Round-2b/decode body.
    fn decode_entries_to_key_val(&mut self) {
        let class_ids: Vec<(u64, u64)> = self
            .all_entry_blobs
            .iter()
            .map(|(&a, &(c, _))| (a, c))
            .collect();
        for (addr, class_id) in class_ids {
            let Some((key_off, value_off, _next_off)) = self.entry_offs(class_id) else {
                continue;
            };
            let Some((_, blob)) = self.all_entry_blobs.get(&addr) else {
                continue;
            };
            if key_off + self.obj_ref_width > blob.len()
                || value_off + self.obj_ref_width > blob.len()
            {
                continue;
            }
            let key_ref = read_ref(&blob[key_off..], self.obj_ref_width);
            let value_ref = read_ref(&blob[value_off..], self.obj_ref_width);
            self.key_val.insert(addr, (key_ref, value_ref));
        }
    }

    /// Decode R3 String blobs into `string_to_arr`. Verbatim from the original
    /// Round-3 String body.
    fn ingest_strings(&mut self, str_blobs: &InstMap) {
        let class_map = &self.p1.class_map;
        let strings = &self.p1.strings;
        let obj_ref_width = self.obj_ref_width;
        let mut str_off_cache: HashMap<u64, Option<(usize, Option<usize>)>> = HashMap::new();
        for (&addr, &(class_id, ref blob)) in str_blobs {
            if !self.wanted_strings.contains(&addr) {
                continue;
            }
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
                        self.string_to_arr.insert(addr, (arr_ref, coder));
                    }
                }
            }
        }
    }
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

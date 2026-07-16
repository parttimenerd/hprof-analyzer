//! Pass 2: the reference-graph core (RSS- and parity-critical).
//!
//! Reads the heap a second time to build the object reference graph: a forward
//! CSR (`fwd_offsets`/`fwd_targets`) and a deferred, blocked+delta-encoded
//! inbound-referrer CSR (`InboundBuilder`). It resolves real GC roots and adds
//! synthetic system-class roots to mirror MAT's `addSystemClassRootsIfMissing`,
//! builds thread stacks/frames and bounded thread-local / alloc-site samples,
//! then hands off to the dominator/retained stages. This is the most
//! memory-sensitive file in the crate (hard peak-RSS budget) and the most
//! parity-sensitive (byte-exact, MAT-frozen counts) — most edits here should be
//! comments, not behavior changes.

use std::{
    collections::HashMap,
    io::{self, ErrorKind},
};

use crate::{
    pass1::Pass1,
    reader::HprofReader,
    types::{HprofType, heap, tags},
};

mod fielddecode;
mod meta;
mod model;
mod scan;
mod sizing;
mod strings;

pub(crate) use fielddecode::ATTRIBUTION_TOP_N;
pub(crate) use fielddecode::{CollDesc, CollKind, builtin_coll_descs};
pub(crate) use meta::*;
pub use model::*;
pub(crate) use scan::*;
pub use sizing::*;
pub use strings::*;

// ── Pass2 main logic ───────────────────────────────────────────────────────

/// Zero-sized entry point for the second parse pass; see [`Pass2::build`].
pub struct Pass2;

impl Pass2 {
    /// Run pass 2 over the dump at `path`, consuming pass1's tables. Detects
    /// ref size, computes MAT shallow sizes, interns the class histogram, scans
    /// the heap twice (degree-count then forward-CSR fill), resolves real +
    /// synthetic GC roots, and captures thread/alloc metadata. Returns the
    /// `Graph`, a deferred `InboundBuilder` (the inbound CSR is built later to
    /// keep its ~5.5GB off the rpo peak), and the early-compressed `shallow` /
    /// `class_idx` blobs. `compress` selects the codec for those cold arrays.
    pub fn build(
        path: &str,
        mut p1: Pass1,
        compress: crate::cvec::Codec,
        opts: &crate::AnalyzeOptions,
    ) -> io::Result<(
        Graph,
        InboundBuilder,
        crate::cvec::CompressedU32,
        crate::cvec::CompressedU32,
        Option<crate::cvec::CompressedU32>,
    )> {
        let n = p1.id_map.len();
        let id_size = p1.id_size;
        let ptr_size = id_size as usize;

        // ── Phase 0: detect ref_size ─────────────────────────────────────
        // Reuse the object-array (addr, count) data already collected in pass1
        // instead of re-scanning the whole file. Array addresses are the id_map
        // entries whose kind == 1 (object array).
        let ref_size = if id_size == 8 {
            let mut array_addr_counts: Vec<(u64, u64)> = Vec::new();
            for i in 0..n {
                if p1.kind[i] == 1 {
                    array_addr_counts.push((p1.id_map.addr_at(i), p1.elem_count[i] as u64));
                }
            }
            detect_ref_size(id_size, &array_addr_counts)
        } else {
            id_size
        } as usize;

        // ── Phase 0b: compute shallow sizes with MAT formula ─────────────
        // Uses per-object kind (0=instance,1=obj_array,2=prim_array,3=class_obj)
        // and raw element counts collected in pass1 — authoritative, no heuristics.
        let mut size_cache: HashMap<u64, usize> = HashMap::new();

        // Set of class-object addresses (used later for edge/class-obj resolution).
        let class_addrs: std::collections::HashSet<u64> = p1.class_map.keys().cloned().collect();

        let mut shallow: Vec<u32> = Vec::with_capacity(n);
        for i in 0..n {
            let cid = p1.class_ids[i];
            let sz = match p1.kind[i] {
                3 => {
                    // Class object: shallow from static fields only, attributed to java.lang.Class.
                    let addr = p1.class_addr_table[cid as usize];
                    match p1.class_map.get(&addr) {
                        Some(ci) => class_obj_shallow(ci, ptr_size, ref_size),
                        None => align_up(ptr_size + ref_size, 8) as u32,
                    }
                }
                1 => {
                    // Object array: cid is the array class index (elem count from pass1).
                    obj_array_shallow(p1.elem_count[i] as u64, ptr_size, ref_size)
                }
                2 => {
                    // Primitive array: cid is the raw element type code.
                    let elem_size = HprofType::from_code(cid as u8)
                        .map(|t| t.byte_size())
                        .unwrap_or(1);
                    prim_array_shallow(p1.elem_count[i] as u64, elem_size, ptr_size, ref_size)
                }
                _ => {
                    // Instance: MAT calculateSizeRecursive over the super chain.
                    let addr = p1.class_addr_table[cid as usize];
                    if p1.class_map.contains_key(&addr) {
                        instance_shallow_size(
                            addr,
                            &p1.class_map,
                            ptr_size,
                            ref_size,
                            &mut size_cache,
                        )
                    } else {
                        align_up(ptr_size + ref_size, 8) as u32
                    }
                }
            };
            shallow.push(sz);
        }

        // ── Phase 0c: Build class names ──────────────────────────────────
        // MAT keys the class histogram by CLASS-OBJECT identity, not by name: a
        // class loaded by two different loaders yields two histogram rows even
        // though the names are identical. We therefore intern by a u64 key:
        //   - instances / object arrays: the class-object address (loader-distinct)
        //   - primitive arrays: PRIM_KEY_BASE | type_code (boot-loaded, single row)
        //   - class objects (java.lang.Class): the JLC_KEY sentinel (single row)
        const PRIM_KEY_BASE: u64 = 0xFFFF_0000_0000_0000;
        const JLC_KEY: u64 = 0xFFFF_FFFF_FFFF_FFFF;
        let mut class_key_to_idx: HashMap<u64, u32> = HashMap::new();
        let mut class_names: Vec<String> = Vec::new();
        let mut class_loader_id: Vec<u64> = Vec::new();

        let mut get_or_insert_class =
            |key: u64, name: &dyn Fn() -> String, loader: &dyn Fn() -> u64| -> u32 {
                if let Some(&idx) = class_key_to_idx.get(&key) {
                    return idx;
                }
                let idx = class_names.len() as u32;
                class_key_to_idx.insert(key, idx);
                class_names.push(name());
                class_loader_id.push(loader());
                idx
            };

        // Build class_idx array
        let mut class_idx: Vec<u32> = vec![0u32; n];
        // class_addr_to_hist: class object address → histogram idx, for instances
        // (and object arrays). Used to build field_plans_dense after the closure drops.
        let mut class_addr_to_hist: HashMap<u64, u32> = HashMap::new();

        // First pass: populate class_idx for all objects (kind-driven, no heuristics)
        for i in 0..n {
            let cid = p1.class_ids[i];

            match p1.kind[i] {
                3 => {
                    // Class object → single java/lang/Class row (MAT parity). Boot-loaded.
                    class_idx[i] =
                        get_or_insert_class(JLC_KEY, &|| "java/lang/Class".to_string(), &|| 0);
                }
                2 => {
                    // Primitive array: cid is the raw element type code. Boot-loaded.
                    let tc = cid as u8;
                    class_idx[i] = get_or_insert_class(
                        PRIM_KEY_BASE | tc as u64,
                        &|| prim_array_class_name(tc).to_string(),
                        &|| 0,
                    );
                }
                1 => {
                    // Object array: cid indexes the array-class address (loader-distinct).
                    let addr = p1.class_addr_table[cid as usize];
                    class_idx[i] = get_or_insert_class(
                        addr,
                        &|| {
                            p1.class_map
                                .get(&addr)
                                .and_then(|ci| p1.strings.get(&ci.name_id).cloned())
                                .unwrap_or_else(|| "[Ljava/lang/Object;".to_string())
                        },
                        &|| p1.class_map.get(&addr).map(|ci| ci.loader_id).unwrap_or(0),
                    );
                    class_addr_to_hist.entry(addr).or_insert(class_idx[i]);
                }
                _ => {
                    // Instance: cid indexes the class-object address (loader-distinct).
                    let addr = p1.class_addr_table[cid as usize];
                    class_idx[i] = get_or_insert_class(
                        addr,
                        &|| {
                            p1.class_map
                                .get(&addr)
                                .and_then(|ci| p1.strings.get(&ci.name_id).cloned())
                                .unwrap_or_else(|| format!("unknown@{addr:#x}"))
                        },
                        &|| p1.class_map.get(&addr).map(|ci| ci.loader_id).unwrap_or(0),
                    );
                    class_addr_to_hist.entry(addr).or_insert(class_idx[i]);
                }
            }
        }

        // Free pass1 per-object arrays that are dead after Phase 0b/0c: they
        // are only read to derive `shallow` and `class_idx` above. Releasing
        // them here (~173 MB for a 11 M-object heap) shrinks peak RSS before
        // the edge-scan allocations (inb_flat / fwd_targets).
        //
        // NOTE: `class_ids`/`kind` are also read by the loader-label resolution
        // loop below, which must run AFTER the `get_or_insert_class` closure is
        // last used (~line 913, it holds a mutable borrow of `class_loader_id`).
        // We therefore keep `class_ids` alive until after that loop and free it
        // there; only the two vecs not needed by the loop are freed here.
        p1.shallow_sizes = Vec::new();
        // ── Fold: arrays-by-size histogram ───────────────────────────────
        // Bucket every array (kind 1=obj, 2=prim) by power-of-two element
        // length into a per-kind BTreeMap keyed by `upper_len`, accumulating
        // object count + shallow bytes. Zero-length arrays are tallied
        // separately. Reuses data already in memory (`shallow` is authoritative
        // for arrays here — Phase 0b computed it with the same MAT formulas the
        // sub-pass 2a scan later re-derives) and runs BEFORE `p1.elem_count` is
        // freed on the next line: no extra scan, RSS/runtime-neutral.
        let arrays_by_size = {
            use crate::report::{ArraysBySize, SizeHistogramBucket};
            use std::collections::BTreeMap;
            let mut obj: BTreeMap<u64, (u64, u64)> = BTreeMap::new();
            let mut prim: BTreeMap<u64, (u64, u64)> = BTreeMap::new();
            let mut zero_length_count: u64 = 0;
            for i in 0..n {
                let k = p1.kind[i];
                if k != 1 && k != 2 {
                    continue;
                }
                let len = p1.elem_count[i] as u64;
                if len == 0 {
                    zero_length_count += 1;
                    continue;
                }
                let upper_len = len.next_power_of_two();
                let map = if k == 1 { &mut obj } else { &mut prim };
                let e = map.entry(upper_len).or_insert((0, 0));
                e.0 += 1;
                e.1 += shallow[i] as u64;
            }
            let to_vec = |m: BTreeMap<u64, (u64, u64)>| -> Vec<SizeHistogramBucket> {
                m.into_iter()
                    .map(|(upper_len, (objects, shallow))| SizeHistogramBucket {
                        upper_len,
                        objects,
                        shallow,
                    })
                    .collect()
            };
            ArraysBySize {
                obj_array_buckets: to_vec(obj),
                prim_array_buckets: to_vec(prim),
                zero_length_count,
            }
        };
        p1.elem_count = Vec::new();

        // ── Phase 1: Sub-pass 2a — count degrees ────────────────────────
        let mut out_degree: Vec<u32> = vec![0u32; n];
        let mut in_degree: Vec<u32> = vec![0u32; n];
        crate::trace::probe("pass2: after out/in_degree alloc");

        // Precompute per-class instance-field plans once (offset + excluded flag).
        // Borrowed immutably in the hot scan loop — no per-instance allocation.
        let field_plans = build_field_plans(&p1.class_map, &p1.strings, id_size as usize);

        // Dense field_plans indexed by histogram class idx — replaces per-object HashMap
        // lookups in the hot scan loops. Sized by the max instance-class histogram index
        // (built from class_addr_to_hist, which is independent of the get_or_insert_class
        // closure so we don't need to borrow class_names or class_key_to_idx here).
        let n_dense_classes = class_idx
            .iter()
            .copied()
            .max()
            .map(|m| m as usize + 1)
            .unwrap_or(0);
let mut field_plans_dense: Vec<FieldPlan> = vec![Vec::new(); n_dense_classes];
        for (&class_addr, &hidx) in &class_addr_to_hist {
            if let Some(plan) = field_plans.get(&class_addr) {
                if !plan.is_empty() {
                    field_plans_dense[hidx as usize] = plan.clone();
                }
            }
        }
        drop(field_plans);

        // ── Sub-pass 2a scan ─────────────────────────────────────────────
        {
            let mut r = HprofReader::open(path)?;
            // Scratch buffer reused across INSTANCE_DUMP and OBJ_ARRAY_DUMP reads (fix #6)
            let mut scratch: Vec<u8> = Vec::with_capacity(4096);

            loop {
                let tag = match r.u1() {
                    Err(e) if e.kind() == ErrorKind::UnexpectedEof => break,
                    other => other?,
                };
                let _ts = r.u4()?;
                let length = r.u4()? as u64;

                match tag {
                    tags::HEAP_DUMP | tags::HEAP_DUMP_SEGMENT => {
                        Self::scan_heap_2a(
                            &mut r,
                            id_size,
                            length,
                            &p1.id_map,
                            &class_addr_to_hist,
                            &field_plans_dense,
                            &mut out_degree,
                            &mut in_degree,
                            &mut scratch,
                        )?;
                    }
                    tags::HEAP_DUMP_END => break,
                    _ => {
                        r.skip(length)?;
                    }
                }
            }
        }

        // Class objects already map to the java/lang/Class row (JLC_KEY) from Phase 0c.
        let jlc_idx = get_or_insert_class(JLC_KEY, &|| "java/lang/Class".to_string(), &|| 0);

        // ── Build class_obj_class_idx ─────────────────────────────────────
        // For each class object, record the histogram row of the class it
        // represents. Under identity keying, that row is keyed by the class
        // object's own address (the same key instances of that class use).
        let mut class_obj_class_idx: HashMap<u32, u32> = HashMap::new();
        for i in 0..n {
            let addr = p1.id_map.addr_at(i);
            if class_addrs.contains(&addr) {
                let ci = p1.class_map.get(&addr);
                let idx = get_or_insert_class(
                    addr,
                    &|| {
                        ci.and_then(|c| p1.strings.get(&c.name_id).cloned())
                            .unwrap_or_else(|| format!("unknown@{addr:#x}"))
                    },
                    &|| ci.map(|c| c.loader_id).unwrap_or(0),
                );
                class_obj_class_idx.insert(i as u32, idx);
            }
        }
        let _ = jlc_idx;

        // Resolve each distinct non-boot class-loader OBJECT address to the
        // class NAME of that loader object (e.g.
        // "jdk/internal/loader/ClassLoaders$AppClassLoader"), so the report
        // layer can label loaders instead of showing a raw address. Runs here,
        // AFTER the last `get_or_insert_class` use (that closure mutably borrows
        // `class_loader_id`), and BEFORE class_map/strings/id_map are freed or
        // moved (~lines 1013-1014, ~1198) and before `class_ids`/`kind` are
        // freed just below. Bounded by #distinct loaders (tens to low
        // hundreds), so it costs no per-object RSS. Boot loader (addr 0) is
        // labeled `<boot>` in the report layer.
        let mut loader_labels: std::collections::HashMap<u64, String> =
            std::collections::HashMap::new();
        for &loader_addr in &class_loader_id {
            if loader_addr == 0 {
                continue; // boot loader handled in report layer
            }
            if loader_labels.contains_key(&loader_addr) {
                continue;
            }
            // Resolve: loader_addr -> object index -> its class-obj addr -> name.
            if let Some(idx) = p1.id_map.index_of(loader_addr) {
                // Only plain instances (kind 0) are real loader objects.
                if p1.kind[idx] == 0 {
                    let cid = p1.class_ids[idx];
                    let class_addr = p1.class_addr_table[cid as usize];
                    if let Some(name) = p1
                        .class_map
                        .get(&class_addr)
                        .and_then(|ci| p1.strings.get(&ci.name_id))
                    {
                        loader_labels.insert(loader_addr, name.clone());
                    }
                }
            }
        }

        // Ensure no zero shallow sizes for instances/arrays (fall back to minimum).
        // Class objects (kind==3) are exempt: MAT reports 0 shallow for a class
        // whose static-field bytes sum to 0 (e.g. array classes like `[I`), so we
        // must not bump those to the object minimum.
        let min_obj = align_up(ptr_size + ref_size, 8) as u32;
        for (i, s) in shallow.iter_mut().enumerate() {
            if *s == 0 && p1.kind[i] != 3 {
                *s = min_obj;
            }
        }

        // ── Phase 2: Build GC root indices ───────────────────────────────
        let mut gc_root_set: std::collections::HashSet<u32> = std::collections::HashSet::new();
        // Per-index representative root type (minimum sub-tag when an index has
        // several root records), carried into Graph for B1 grouping + why-alive.
        let mut root_type_of: std::collections::HashMap<u32, u8> = std::collections::HashMap::new();
        let note_type = |m: &mut std::collections::HashMap<u32, u8>, idx: u32, ty: u8| {
            m.entry(idx).and_modify(|e| *e = (*e).min(ty)).or_insert(ty);
        };
        for (&addr, &ty) in p1.gc_root_addrs.iter().zip(p1.gc_root_types.iter()) {
            if let Some(idx) = p1.id_map.index_of(addr) {
                gc_root_set.insert(idx as u32);
                note_type(&mut root_type_of, idx as u32, ty);
            }
        }
        // Add implicit roots: non-array boot-loader classes (loader_id==0) if no sticky roots
        if !p1.has_sticky_class_roots {
            for (&caddr, ci) in &p1.class_map {
                if ci.loader_id == 0 {
                    // Check it's not an array class (name doesn't start with '[')
                    let is_array = p1
                        .strings
                        .get(&ci.name_id)
                        .map(|n| n.starts_with('['))
                        .unwrap_or(false);
                    if !is_array {
                        if let Some(idx) = p1.id_map.index_of(caddr) {
                            gc_root_set.insert(idx as u32);
                            note_type(&mut root_type_of, idx as u32, heap::ROOT_SYSTEM_CLASS);
                        }
                    }
                }
            }
        }
        // ── addSystemClassRootsIfMissing: boot-loader non-array classes not yet roots ─
        let mut synthetic_root_count = 0usize;
        // MAT materializes a synthetic <system class loader> object at 0x0 of
        // class java/lang/ClassLoader (no HPROF record). Capture that class's
        // instance shallow size so the report layer can inject the object.
        let mut system_classloader_shallow: Option<u32> = None;
        for (&caddr, ci) in &p1.class_map {
            if ci.loader_id != 0 {
                continue;
            }
            let name = p1.strings.get(&ci.name_id);
            if name.map(|n| n == "java/lang/ClassLoader").unwrap_or(false) {
                system_classloader_shallow = Some(instance_shallow_size(
                    caddr,
                    &p1.class_map,
                    ptr_size,
                    ref_size,
                    &mut size_cache,
                ));
            }
            let is_array = name.map(|n| n.starts_with('[')).unwrap_or(false);
            let is_prim_array = name
                .map(|n| is_primitive_array_class_name(n))
                .unwrap_or(false);
            if !should_add_system_class_root(is_array, is_prim_array, p1.has_sticky_class_roots) {
                continue;
            }
            if let Some(idx) = p1.id_map.index_of(caddr) {
                if !gc_root_set.contains(&(idx as u32)) {
                    gc_root_set.insert(idx as u32);
                    note_type(&mut root_type_of, idx as u32, heap::ROOT_SYSTEM_CLASS);
                    synthetic_root_count += 1;
                }
            }
        }

        // Resolve STACK_TRACE/STACK_FRAME into pre-rendered thread stacks while
        // pass1's string/class tables are still alive (they are freed just
        // below). Only traces that carry frames are kept. Small — one entry per
        // thread trace, off the per-object RSS budget.
        let thread_stacks = build_thread_stacks(&p1);

        // Pre-resolve every DISTINCT non-zero alloc stack-trace serial into its
        // frame lines while the STACK_FRAME/STACK_TRACE + string/class tables
        // are still alive (freed just below). Bounded by the number of distinct
        // traces (hundreds), so it stays off the per-object RSS budget.
        let alloc_frames_by_serial: Option<std::collections::HashMap<u32, Vec<String>>> =
            Some(resolve_alloc_frames(&p1));

        // Decode each thread's java.lang.Thread.name via a bounded 3-pass
        // worklist, while class_map/strings/id_map are still alive (freed just
        // below). All captured sets are bounded by #threads, so this stays off
        // the per-object RSS budget on multi-GB dumps.
        let thread_props = resolve_thread_names(path, &p1)?;

        // Opt-in approximate duplicate-java.lang.String report. Runs two extra
        // full-file scans and keeps only hashes+lengths+counts (never the
        // decoded bytes), so RSS stays bounded. Must run while class_map/strings
        // are still alive (freed just below). `None` on the default path = zero
        // extra work, zero RSS.
        let dup_strings = if opts.dup_strings {
            Some(resolve_duplicate_strings(path, &p1)?)
        } else {
            None
        };

        // Capture java.lang.System's static `props` (a Properties/Hashtable of
        // String->String) via a bounded multi-pass worklist, while class_map/
        // strings/id_map are still alive. All captured sets are bounded (ONE
        // props object, capped at 4096 entries + their Strings/arrays), so this
        // stays off the per-object RSS budget on multi-GB dumps. Derives a JVM
        // version from the decoded properties. Falls back to empty/None (never
        // garbage) if the layout does not match the Hashtable form.
        let (system_properties, jvm_version) = resolve_system_properties(path, &p1)?;

        // Always-on field-decode views (collections, arrays, references). One
        // shared 3-scan pass; all aggregates are capped (see fielddecode.rs), so
        // RSS stays within the grant. Must run while class_map/strings are alive.
        let (
            fd_collections,
            fd_references,
            fd_referent_idx,
            fd_attribution_raw,
            fd_attribution_trunc,
            fd_dbb_capacity_sum,
        ) = fielddecode::build_field_decode_views(
            path,
            &p1,
            &shallow,
            opts.collections,
            &opts.coll_descs,
        )?;

        // Free class_ids now: build_field_decode_views was its last reader
        // (class_name_of_index uses it for referent class lookups). Releasing
        // here keeps peak RSS low before the edge-scan allocations.
        p1.class_ids = Vec::new();

        // class_map + strings are no longer needed; free before the large edge
        // arrays get allocated in Phase 3/4 to lower peak RSS. The STACK_FRAME/
        // STACK_TRACE maps were just consumed by build_thread_stacks and are
        // likewise dead — free them here too so they don't linger through the
        // peak-binding dominator/retained phases.
        p1.class_map = std::collections::HashMap::new();
        p1.strings = std::collections::HashMap::default();
        p1.stack_frames = std::collections::HashMap::default();
        p1.stack_traces = std::collections::HashMap::new();
        p1.stack_trace_thread = std::collections::HashMap::default();

        // ── Resolve thread→local synthetic edges ─────────────────────────
        let mut synthetic_edges: Vec<(u32, u32)> = Vec::new();
        // Per-thread count of local roots that resolve to a live object. Sized
        // by #threads only (bounded), so it stays off the per-object RSS budget.
        let mut thread_local_counts: std::collections::HashMap<u32, u64> =
            std::collections::HashMap::new();
        // Bounded per-thread sample of local object indices. Only populated when
        // the opt-in `--thread-locals` flag is set; empty otherwise (zero cost on
        // the default path).
        let mut thread_local_samples: std::collections::HashMap<u32, Vec<u32>> =
            std::collections::HashMap::new();
        // Gated frame→local map: per-thread (frame_number, local_idx) pairs, used
        // to build MAT's per-frame significant-locals interleave. Only populated
        // when `--thread-locals` is set; `u32::MAX` frame_number = no frame (JNI
        // local / native stack / thread block). Bounded per thread by the same
        // per-thread cap. Zero cost on the default path.
        let mut thread_local_frame_samples: std::collections::HashMap<u32, Vec<(u32, u32)>> =
            std::collections::HashMap::new();
        for &(thread_serial, frame_number, local_addr) in &p1.thread_local_pairs {
            let thread_obj_addr = match p1.thread_serial_to_obj_id.get(&thread_serial) {
                Some(&a) => a,
                None => continue,
            };
            let thread_idx = match p1.id_map.index_of(thread_obj_addr) {
                Some(i) => i as u32,
                None => continue,
            };
            let local_idx = match p1.id_map.index_of(local_addr) {
                Some(i) => i as u32,
                None => continue,
            };
            if thread_idx != local_idx {
                synthetic_edges.push((thread_idx, local_idx));
                *thread_local_counts.entry(thread_serial).or_insert(0) += 1;
                let sample = thread_local_samples.entry(thread_serial).or_default();
                if sample.len() < opts.thread_locals_per_thread {
                    sample.push(local_idx);
                }
                if opts.thread_locals_per_thread > 0 {
                    let fs = thread_local_frame_samples.entry(thread_serial).or_default();
                    if fs.len() < opts.thread_locals_per_thread {
                        fs.push((frame_number, local_idx));
                    }
                }
            }
        }
        // Dedup synthetic edges (same thread may reference same local multiple times)
        synthetic_edges.sort_unstable();
        synthetic_edges.dedup();

        // Add synthetic edge degrees to out_degree/in_degree
        for &(src, dst) in &synthetic_edges {
            out_degree[src as usize] += 1;
            in_degree[dst as usize] += 1;
        }

        let mut gc_root_indices: Vec<u32> = gc_root_set.into_iter().collect();
        gc_root_indices.sort_unstable();
        // Per-root type aligned 1:1 with the sorted indices. Every index in the
        // set came from a note_type call, so the lookup always hits; fall back
        // to ROOT_UNKNOWN defensively.
        let gc_root_types: Vec<u8> = gc_root_indices
            .iter()
            .map(|idx| root_type_of.get(idx).copied().unwrap_or(heap::ROOT_UNKNOWN))
            .collect();

        // Compress the two cold per-object arrays (shallow, class_idx) NOW,
        // before the forward-CSR fwd_targets (~6GB) is allocated. Both are
        // final here and idle until the retained phase, so freeing their dense
        // ~2GB Vecs each removes ~4GB from the binding fwd_targets-alloc peak.
        // main.rs holds the blobs across the peak and restores before consumers.
        let shallow_c = crate::cvec::CompressedU32::compress(&shallow, compress)?;
        if compress != crate::cvec::Codec::None {
            shallow = Vec::new();
        }
        let class_idx_c = crate::cvec::CompressedU32::compress(&class_idx, compress)?;
        if compress != crate::cvec::Codec::None {
            class_idx = Vec::new();
        }
        // Compress the per-object alloc stack serials (~2GB dense u32 under
        // alloc-sites) at the SAME point, before fwd_targets alloc: pass1
        // touched every element (faulting all pages), so even a mostly-zero
        // array occupies ~2GB real RSS through the fwd_targets + rpo + inbound
        // binding peak. It is read only once, by the report's alloc-site
        // aggregation, long after that peak. main.rs holds the blob and streams
        // it back post-retained (see build_alloc_sites_from). None only under
        // Codec::None (the raw array is kept for the direct-aggregate path).
        // Graph.alloc_stack_serial stays empty.
        let alloc_serial_c = if compress != crate::cvec::Codec::None {
            let c = crate::cvec::CompressedU32::compress(&p1.alloc_stack_serial, compress)?;
            p1.alloc_stack_serial = Vec::new();
            Some(c)
        } else {
            None
        };
        crate::trace::probe("pass2: after early-compress shallow/class_idx");

        // ── Phase 3: Build forward-CSR offsets (prefix sum only) ────────
        let mut fwd_offsets: Vec<u32> = Vec::with_capacity(n + 1);
        fwd_offsets.push(0u32);
        for i in 0..n {
            let next = fwd_offsets[i] + out_degree[i];
            fwd_offsets.push(next);
        }
        drop(out_degree); // dead after prefix sum
        crate::trace::probe("pass2: after fwd_offsets prefix-sum (out_degree freed)");

        // ── Phase 3b: Build forward CSR ──────────────────────────────────
        // The forward fill runs FIRST (inside build); the inbound CSR is
        // deferred into InboundBuilder so its ~5.5GB does not coexist with
        // the rpo phase's arrays. The forward fill never touches inb_flat.
        let total_edges = *fwd_offsets.last().unwrap() as usize;
        let mut fwd_targets: Vec<u32> = vec![u32::MAX; total_edges];
        crate::trace::probe("pass2: after fwd_targets alloc");
        // B3: no fwd_cursor clone. fwd_offsets is advanced in place as the
        // write cursor during the fill, then restored by right-shift below.
        {
            let mut r = HprofReader::open(path)?;
            let mut scratch: Vec<u8> = Vec::with_capacity(4096);
            let mut inb_flat_stub = crate::chunkvec::ChunkU32::zeroed(0);
            let mut in_degree_stub: Vec<u32> = Vec::new();
            loop {
                let tag = match r.u1() {
                    Err(e) if e.kind() == ErrorKind::UnexpectedEof => break,
                    other => other?,
                };
                let _ts = r.u4()?;
                let length = r.u4()? as u64;
                match tag {
                    tags::HEAP_DUMP | tags::HEAP_DUMP_SEGMENT => {
                        Self::fill_heap_2b(
                            &mut r,
                            id_size,
                            length,
                            &p1.id_map,
                            &class_addr_to_hist,
                            &field_plans_dense,
                            true,
                            false,
                            &mut fwd_targets,
                            &mut fwd_offsets,
                            &mut inb_flat_stub,
                            &mut in_degree_stub,
                            &mut scratch,
                        )?;
                    }
                    tags::HEAP_DUMP_END => break,
                    _ => {
                        r.skip(length)?;
                    }
                }
            }
        }
        // Synthetic thread->local FORWARD edges. Their degrees were added to
        // out_degree above, so each fits within its node's slice.
        for &(src, dst) in &synthetic_edges {
            let pos = fwd_offsets[src as usize] as usize;
            fwd_targets[pos] = dst;
            fwd_offsets[src as usize] += 1;
        }

        // B3 restore: each fwd_offsets[i] (i in 0..n) has advanced to node i's
        // END index; right-shift over (1..=n).rev() so fwd_offsets[node]..
        // fwd_offsets[node+1] again bounds node's slice. fwd_offsets[n]
        // (total_edges) was never a cursor and is preserved by starting at n.
        for i in (1..=n).rev() {
            fwd_offsets[i] = fwd_offsets[i - 1];
        }
        fwd_offsets[0] = 0;

        // Prefix-sum in_degree counts → START cursors for the deferred inbound
        // build. in_degree[i] becomes node i's inbound slice START; total_inb
        // is the flat inbound length. inb_flat is NOT allocated here.
        let mut total_inb: u64 = 0;
        for d in in_degree.iter_mut() {
            let cnt = *d as u64;
            *d = total_inb as u32;
            total_inb += cnt;
        }
        let in_cursors = in_degree; // renamed for clarity: prefix-summed START cursors

        // Precompute source_name before moving p1.id_map into InboundBuilder.
        let source_name = std::path::Path::new(path)
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string());

        let alloc_stack_serial = std::mem::take(&mut p1.alloc_stack_serial);
        let mut gc_root_tag_counts: Vec<(u8, u64)> = p1
            .gc_root_tag_counts
            .iter()
            .map(|(&t, &c)| (t, c))
            .collect();
        gc_root_tag_counts.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        let record_census = RecordCensus {
            utf8_records: p1.utf8_records,
            load_class_records: p1.load_class_records,
            unload_class_records: p1.unload_class_records,
            stack_frame_records: p1.stack_frame_records,
            stack_trace_records: p1.stack_trace_records,
            heap_dump_segments: p1.heap_dump_segments,
            instance_dumps: p1.instance_count,
            obj_array_dumps: p1.obj_array_count,
            prim_array_dumps: p1.prim_array_count,
            class_dumps: p1.class_dump_count,
            gc_root_tag_counts,
        };
        let graph = Graph {
            n,
            format: p1.format,
            file_size: p1.file_size,
            source_name,
            file_path: path.to_string(),
            id_size,
            ref_size: ref_size as u8,
            header_timestamp_ms: p1.header_timestamp_ms,
            gc_root_indices,
            gc_root_types,
            shallow,
            class_idx,
            class_names,
            class_loader_id,
            loader_labels,
            thread_stacks,
            thread_props,
            thread_local_counts,
            thread_local_samples,
            thread_local_frame_samples,
            system_properties,
            jvm_version,
            class_obj_class_idx,
            fwd_offsets,
            fwd_targets,
            synthetic_root_count,
            system_classloader_shallow,
            idom: Vec::new(),
            retained: Vec::new(),
            has_same_class_ancestor: crate::bitset::Bitset::default(),
            alloc_stack_serial,
            alloc_frames_by_serial,
            record_census,
            dup_strings,
            arrays_by_size,
            collections: fd_collections,
            references: fd_references,
            reference_referent_idx: fd_referent_idx,
            collection_attribution_raw: fd_attribution_raw,
            collection_attribution_truncated: fd_attribution_trunc,
            direct_byte_buffer_capacity_sum: fd_dbb_capacity_sum,
        };

        // Package the deferred inbound-CSR construction. Moves id_map,
        // class_addrs, field_plans and synthetic_edges out of build (all
        // unused here after the forward fill).
        let inbound = InboundBuilder {
            path: path.to_string(),
            id_size,
            n,
            id_map: Some(p1.id_map),
            id_map_c: None,
            id_map_codec: crate::cvec::Codec::None,
            // build_from_fwd drops these immediately; build() (file-scan path) is unused.
            class_addr_to_hist: HashMap::new(),
            field_plans_dense: Vec::new(),
            in_cursors,
            total_inb,
            synthetic_edges,
        };

        Ok((graph, inbound, shallow_c, class_idx_c, alloc_serial_c))
    }

    /// First-scan heap walker that COUNTS out/in degrees per node and finalizes
    /// each object's authoritative shallow size (arrays/instances use their real
    /// element count / class blob). Produces the degree arrays that Phase 3
    /// prefix-sums into the CSR offsets; fills no edge targets itself.
    #[allow(clippy::too_many_arguments)]
    fn scan_heap_2a(
        r: &mut HprofReader,
        id_size: u8,
        mut remaining: u64,
        id_map: &crate::id_map::IdMap,
        class_addr_to_hist: &HashMap<u64, u32>,
        field_plans_dense: &[FieldPlan],
        out_degree: &mut Vec<u32>,
        in_degree: &mut Vec<u32>,
        scratch: &mut Vec<u8>,
    ) -> io::Result<()> {
        let ids = id_size as u64;
        let mut cache = crate::id_map::IndexCache::new();

        macro_rules! edge_if_valid {
            ($src:expr, $dst_addr:expr, $excl:expr) => {
                if $dst_addr != 0 {
                    if let Some(dst) = cache.index_of(id_map, $dst_addr) {
                        let src = $src as usize;
                        out_degree[src] += 1;
                        in_degree[dst] += 1;
                    }
                }
            };
        }

        macro_rules! checked_sub {
            ($remaining:expr, $sz:expr) => {
                $remaining = $remaining
                    .checked_sub($sz)
                    .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "segment overrun"))?;
            };
        }

        while remaining > 0 {
            let sub_tag = r.u1()?;
            checked_sub!(remaining, 1u64);

            match sub_tag {
                heap::ROOT_UNKNOWN | heap::ROOT_MONITOR_USED => {
                    r.skip(ids)?;
                    checked_sub!(remaining, ids);
                }
                heap::ROOT_JNI_GLOBAL => {
                    r.skip(2 * ids)?;
                    checked_sub!(remaining, 2 * ids);
                }
                heap::ROOT_JNI_LOCAL | heap::ROOT_JAVA_FRAME => {
                    r.skip(ids + 8)?;
                    checked_sub!(remaining, ids + 8);
                }
                heap::ROOT_NATIVE_STACK | heap::ROOT_THREAD_BLOCK => {
                    r.skip(ids + 4)?;
                    checked_sub!(remaining, ids + 4);
                }
                heap::ROOT_STICKY_CLASS => {
                    r.skip(ids)?;
                    checked_sub!(remaining, ids);
                }
                heap::ROOT_THREAD_OBJ => {
                    r.skip(ids + 8)?;
                    checked_sub!(remaining, ids + 8);
                }
                heap::CLASS_DUMP => {
                    let consumed =
                        Self::count_class_dump_edges(r, id_size, id_map, out_degree, in_degree)?;
                    checked_sub!(remaining, consumed);
                }
                heap::INSTANCE_DUMP => {
                    let addr = r.id()?;
                    r.skip(4)?;
                    let class_id = r.id()?;
                    let data_len = r.u4()? as u64;
                    if data_len > remaining {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "array too large",
                        ));
                    }
                    r.read_bytes_reuse(scratch, data_len as usize)?;
                    checked_sub!(remaining, ids + 4 + ids + 4 + data_len);

                    let src_idx = match id_map.index_of(addr) {
                        Some(i) => i,
                        None => continue,
                    };

                    // Edge: instance → class object
                    edge_if_valid!(src_idx, class_id, false);

                    // Edges from Object-type fields (dense Vec by class histogram idx,
                    // no HashMap lookup — Phase 0b already precomputed the per-class plan).
                    if let Some(&cidx) = class_addr_to_hist.get(&class_id) {
                        for &(off, _excluded) in &field_plans_dense[cidx as usize] {
                            let off = off as usize;
                            if off + id_size as usize <= scratch.len() {
                                let ref_val = read_ref(&scratch[off..], id_size as usize);
                                if ref_val != 0 {
                                    if let Some(dst) = cache.index_of(id_map, ref_val) {
                                        out_degree[src_idx] += 1;
                                        in_degree[dst] += 1;
                                    }
                                }
                            }
                        }
                    }
                }
                heap::OBJ_ARRAY_DUMP => {
                    let addr = r.id()?;
                    r.skip(4)?;
                    let count = r.u4()? as u64;
                    let elem_class_id = r.id()?;
                    let byte_len = count.saturating_mul(ids);
                    if byte_len > remaining {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "array too large",
                        ));
                    }
                    r.read_bytes_reuse(scratch, byte_len as usize)?;
                    checked_sub!(remaining, ids + 4 + 4 + ids + byte_len);

                    let src_idx = match id_map.index_of(addr) {
                        Some(i) => i,
                        None => continue,
                    };

                    // Shallow size already correct from Phase 0b; class_idx set by Phase 0c.

                    // Edge: array → element class object
                    edge_if_valid!(src_idx, elem_class_id, false);

                    // Edges: array → non-null elements
                    for chunk in scratch.chunks(ids as usize) {
                        let ref_val = read_id(chunk, id_size);
                        if ref_val != 0 {
                            if let Some(dst) = cache.index_of(id_map, ref_val) {
                                out_degree[src_idx] += 1;
                                in_degree[dst] += 1;
                            }
                        }
                    }
                }
                heap::PRIM_ARRAY_DUMP => {
                    let _addr = r.id()?;
                    r.skip(4)?;
                    let count = r.u4()? as u64;
                    let elem_type = r.u1()?;
                    let esz = HprofType::from_code(elem_type)
                        .map(|t| t.byte_size() as u64)
                        .unwrap_or(1);
                    r.skip(count * esz)?;
                    checked_sub!(remaining, ids + 4 + 4 + 1 + count * esz);
                    // No object edges; shallow already set by Phase 0b.
                }
                other => {
                    return Err(io::Error::new(
                        ErrorKind::InvalidData,
                        format!("unknown heap sub-tag 0x{other:02x} in 2a"),
                    ));
                }
            }
        }
        Ok(())
    }

    /// Second-scan heap walker that FILLS the CSR edge arrays (degrees already
    /// counted by `scan_heap_2a`). `do_fwd`/`do_inb` select which side is being
    /// filled: the forward pass advances `fwd_offsets` in place as write
    /// cursors; the inbound pass writes into `inb_flat` at `in_degree` cursors,
    /// tagging excluded (weak/finalizer) referrers with the high bit.
    #[allow(clippy::too_many_arguments)]
    fn fill_heap_2b(
        r: &mut HprofReader,
        id_size: u8,
        mut remaining: u64,
        id_map: &crate::id_map::IdMap,
        class_addr_to_hist: &HashMap<u64, u32>,
        field_plans_dense: &[FieldPlan],
        do_fwd: bool,
        do_inb: bool,
        fwd_targets: &mut Vec<u32>,
        fwd_offsets: &mut Vec<u32>,
        inb_flat: &mut crate::chunkvec::ChunkU32,
        in_degree: &mut Vec<u32>,
        scratch: &mut Vec<u8>,
    ) -> io::Result<()> {
        let ids = id_size as u64;
        let mut cache = crate::id_map::IndexCache::new();

        macro_rules! add_edge {
            ($src:expr, $dst_addr:expr, $excluded:expr) => {
                if $dst_addr != 0 {
                    if let Some(dst) = cache.index_of(id_map, $dst_addr) {
                        let src = $src as usize;
                        if do_fwd {
                            // fwd_offsets[src] is the in-place write cursor.
                            let pos = fwd_offsets[src] as usize;
                            fwd_targets[pos] = dst as u32;
                            fwd_offsets[src] += 1;
                        }
                        if do_inb {
                            // Inbound: store src with/without exclusion flag
                            let inb_val = if $excluded {
                                (src as u32) | 0x8000_0000u32
                            } else {
                                src as u32
                            };
                            inb_flat.set(in_degree[dst] as usize, inb_val);
                            in_degree[dst] += 1;
                        }
                    }
                }
            };
        }

        macro_rules! checked_sub {
            ($remaining:expr, $sz:expr) => {
                $remaining = $remaining
                    .checked_sub($sz)
                    .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "segment overrun"))?;
            };
        }

        while remaining > 0 {
            let sub_tag = r.u1()?;
            checked_sub!(remaining, 1u64);

            match sub_tag {
                heap::ROOT_UNKNOWN | heap::ROOT_MONITOR_USED => {
                    r.skip(ids)?;
                    checked_sub!(remaining, ids);
                }
                heap::ROOT_JNI_GLOBAL => {
                    r.skip(2 * ids)?;
                    checked_sub!(remaining, 2 * ids);
                }
                heap::ROOT_JNI_LOCAL | heap::ROOT_JAVA_FRAME => {
                    r.skip(ids + 8)?;
                    checked_sub!(remaining, ids + 8);
                }
                heap::ROOT_NATIVE_STACK | heap::ROOT_THREAD_BLOCK => {
                    r.skip(ids + 4)?;
                    checked_sub!(remaining, ids + 4);
                }
                heap::ROOT_STICKY_CLASS => {
                    r.skip(ids)?;
                    checked_sub!(remaining, ids);
                }
                heap::ROOT_THREAD_OBJ => {
                    r.skip(ids + 8)?;
                    checked_sub!(remaining, ids + 8);
                }
                heap::CLASS_DUMP => {
                    let consumed = Self::fill_class_dump_edges(
                        r,
                        id_size,
                        id_map,
                        do_fwd,
                        do_inb,
                        fwd_targets,
                        fwd_offsets,
                        inb_flat,
                        in_degree,
                    )?;
                    checked_sub!(remaining, consumed);
                }
                heap::INSTANCE_DUMP => {
                    let addr = r.id()?;
                    r.skip(4)?;
                    let class_id = r.id()?;
                    let data_len = r.u4()? as u64;
                    if data_len > remaining {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "array too large",
                        ));
                    }
                    r.read_bytes_reuse(scratch, data_len as usize)?;
                    checked_sub!(remaining, ids + 4 + ids + 4 + data_len);

                    let src_idx = match id_map.index_of(addr) {
                        Some(i) => i,
                        None => continue,
                    };

                    // Edge: instance → class object
                    add_edge!(src_idx, class_id, false);

                    // Edges from Object-type fields (dense Vec by class histogram idx)
                    if let Some(&cidx) = class_addr_to_hist.get(&class_id) {
                        for &(off, excluded) in &field_plans_dense[cidx as usize] {
                            let off = off as usize;
                            if off + id_size as usize <= scratch.len() {
                                let ref_val = read_ref(&scratch[off..], id_size as usize);
                                add_edge!(src_idx, ref_val, excluded);
                            }
                        }
                    }
                }
                heap::OBJ_ARRAY_DUMP => {
                    let addr = r.id()?;
                    r.skip(4)?;
                    let count = r.u4()? as u64;
                    let elem_class_id = r.id()?;
                    let byte_len = count.saturating_mul(ids);
                    if byte_len > remaining {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "array too large",
                        ));
                    }
                    r.read_bytes_reuse(scratch, byte_len as usize)?;
                    checked_sub!(remaining, ids + 4 + 4 + ids + byte_len);

                    let src_idx = match id_map.index_of(addr) {
                        Some(i) => i,
                        None => continue,
                    };

                    // Edge: array → element class
                    add_edge!(src_idx, elem_class_id, false);

                    // Edges to elements
                    for chunk in scratch.chunks(ids as usize) {
                        let ref_val = read_id(chunk, id_size);
                        add_edge!(src_idx, ref_val, false);
                    }
                }
                heap::PRIM_ARRAY_DUMP => {
                    let _addr = r.id()?;
                    r.skip(4)?;
                    let count = r.u4()? as u64;
                    let elem_type = r.u1()?;
                    let esz = HprofType::from_code(elem_type)
                        .map(|t| t.byte_size() as u64)
                        .unwrap_or(1);
                    r.skip(count * esz)?;
                    checked_sub!(remaining, ids + 4 + 4 + 1 + count * esz);
                    // No object edges from prim arrays
                }
                other => {
                    return Err(io::Error::new(
                        ErrorKind::InvalidData,
                        format!("unknown heap sub-tag 0x{other:02x} in 2b"),
                    ));
                }
            }
        }
        Ok(())
    }

    /// FILL-phase counterpart to `count_class_dump_edges`: emits a class
    /// object's structural edges (→ superclass, → loader, → each Object-typed
    /// static field) into the forward and/or inbound CSR. Returns bytes consumed.
    #[allow(clippy::too_many_arguments)]
    fn fill_class_dump_edges(
        r: &mut HprofReader,
        id_size: u8,
        id_map: &crate::id_map::IdMap,
        do_fwd: bool,
        do_inb: bool,
        fwd_targets: &mut Vec<u32>,
        fwd_offsets: &mut Vec<u32>,
        inb_flat: &mut crate::chunkvec::ChunkU32,
        in_degree: &mut Vec<u32>,
    ) -> io::Result<u64> {
        let ids = id_size as u64;
        let mut consumed = 0u64;

        let class_addr = r.id()?;
        consumed += ids;
        r.skip(4)?;
        consumed += 4;
        let super_id = r.id()?;
        consumed += ids;
        let loader_id = r.id()?;
        consumed += ids;
        r.skip(ids * 4 + 4)?;
        consumed += ids * 4 + 4;

        let src_idx_opt = id_map.index_of(class_addr);

        macro_rules! add_edge_inner {
            ($src:expr, $dst_addr:expr) => {
                if $dst_addr != 0 {
                    if let Some(dst) = id_map.index_of($dst_addr) {
                        let src = $src as usize;
                        if do_fwd {
                            // fwd_offsets[src] is the in-place write cursor.
                            let pos = fwd_offsets[src] as usize;
                            fwd_targets[pos] = dst as u32;
                            fwd_offsets[src] += 1;
                        }
                        if do_inb {
                            inb_flat.set(in_degree[dst] as usize, src as u32);
                            in_degree[dst] += 1;
                        }
                    }
                }
            };
        }

        if let Some(src) = src_idx_opt {
            add_edge_inner!(src, super_id);
            add_edge_inner!(src, loader_id);
        }

        // Constant pool
        let cp = r.u2()? as u64;
        consumed += 2;
        for _ in 0..cp {
            r.skip(2)?;
            consumed += 2;
            let tp = r.u1()?;
            consumed += 1;
            let vs = value_size(tp, id_size);
            r.skip(vs)?;
            consumed += vs;
        }

        // Static fields
        let sc = r.u2()? as u64;
        consumed += 2;
        for _ in 0..sc {
            r.skip(ids)?;
            consumed += ids; // name_id
            let tp = r.u1()?;
            consumed += 1;
            let vs = value_size(tp, id_size);
            if tp == 2 {
                // Object static field
                let ref_val = read_id_from_reader(r, id_size)?;
                consumed += vs;
                if let Some(src) = src_idx_opt {
                    add_edge_inner!(src, ref_val);
                }
            } else {
                r.skip(vs)?;
                consumed += vs;
            }
        }

        // Instance fields (just skip)
        let ic = r.u2()? as u64;
        consumed += 2;
        r.skip(ic * (ids + 1))?;
        consumed += ic * (ids + 1);

        Ok(consumed)
    }
}

// ── Also need 2a version of CLASS_DUMP to count static obj edges ───────────
// We need a version that also counts degrees for CLASS_DUMP static fields.

impl Pass2 {
    /// COUNT-phase counterpart to `fill_class_dump_edges`: counts a class
    /// object's structural edges (→ superclass, → loader, → each Object-typed
    /// static field) into the degree arrays. Returns bytes consumed.
    fn count_class_dump_edges(
        r: &mut HprofReader,
        id_size: u8,
        id_map: &crate::id_map::IdMap,
        out_degree: &mut Vec<u32>,
        in_degree: &mut Vec<u32>,
    ) -> io::Result<u64> {
        let ids = id_size as u64;
        let mut consumed = 0u64;

        let class_addr = r.id()?;
        consumed += ids;
        r.skip(4)?;
        consumed += 4;
        let super_id = r.id()?;
        consumed += ids;
        let loader_id = r.id()?;
        consumed += ids;
        r.skip(ids * 4 + 4)?;
        consumed += ids * 4 + 4;

        let src_opt = id_map.index_of(class_addr);

        macro_rules! count_edge {
            ($dst_addr:expr) => {
                if $dst_addr != 0 {
                    if let Some(dst) = id_map.index_of($dst_addr) {
                        if let Some(src) = src_opt {
                            out_degree[src] += 1;
                            in_degree[dst] += 1;
                        }
                    }
                }
            };
        }

        if src_opt.is_some() {
            count_edge!(super_id);
            count_edge!(loader_id);
        }

        let cp = r.u2()? as u64;
        consumed += 2;
        for _ in 0..cp {
            r.skip(2)?;
            consumed += 2;
            let tp = r.u1()?;
            consumed += 1;
            let vs = value_size(tp, id_size);
            r.skip(vs)?;
            consumed += vs;
        }

        let sc = r.u2()? as u64;
        consumed += 2;
        for _ in 0..sc {
            r.skip(ids)?;
            consumed += ids;
            let tp = r.u1()?;
            consumed += 1;
            let vs = value_size(tp, id_size);
            if tp == 2 {
                let ref_val = read_id_from_reader(r, id_size)?;
                consumed += vs;
                count_edge!(ref_val);
            } else {
                r.skip(vs)?;
                consumed += vs;
            }
        }

        let ic = r.u2()? as u64;
        consumed += 2;
        r.skip(ic * (ids + 1))?;
        consumed += ic * (ids + 1);

        Ok(consumed)
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pass1::{ClassInfo, Pass1};

    const DUMP: &str = "tests/fixtures/dump_0_fj-kmeans.hprof";

    #[test]
    fn pretty_binary_name_strips_l_and_semicolon_and_dots() {
        assert_eq!(pretty_binary_name("Lfoo/bar/Baz;"), "foo.bar.Baz");
        assert_eq!(pretty_binary_name("foo/bar/Baz"), "foo.bar.Baz");
        assert_eq!(pretty_binary_name("Baz"), "Baz");
    }

    #[test]
    fn render_frame_applies_hprof_line_conventions() {
        assert_eq!(
            render_frame(Some("foo.Bar"), Some("run"), Some("Bar.java"), 7, 42),
            "foo.Bar.run (Bar.java:42)"
        );
        assert_eq!(
            render_frame(Some("foo.Bar"), Some("run"), Some("Bar.java"), 7, -1),
            "foo.Bar.run (Bar.java)"
        );
        assert_eq!(
            render_frame(Some("foo.Bar"), Some("run"), Some("Bar.java"), 7, -2),
            "foo.Bar.run (Bar.java(Compiled Method))"
        );
        assert_eq!(
            render_frame(Some("foo.Bar"), Some("run"), Some("Bar.java"), 7, -3),
            "foo.Bar.run (Native Method)"
        );
    }

    #[test]
    fn render_frame_falls_back_when_strings_missing() {
        assert_eq!(
            render_frame(None, None, None, 99, -1),
            "<class#99>.<method> (Unknown Source)"
        );
    }

    #[test]
    fn pass2_graph_has_edges() {
        if !std::path::Path::new(DUMP).exists() {
            return;
        }
        let p1 = Pass1::run(DUMP).unwrap();
        let (g, inbound, _sc, _ci, _as) = Pass2::build(
            DUMP,
            p1,
            crate::cvec::Codec::None,
            &crate::AnalyzeOptions::default(),
        )
        .unwrap();
        assert!(!g.fwd_targets.is_empty(), "no forward edges");
        assert_eq!(g.fwd_offsets.len(), g.n + 1);
        // Identity dfn (node -> pre-order) suffices. build() now returns
        // blocked offsets: one sampled offset per INB_BLOCK nodes + a trailing
        // sentinel, so len == ceil(n / INB_BLOCK) + 1.
        let dfn: Vec<u32> = (0..g.n as u32).collect();
        let (inb_block_off, _inb_data) = inbound.build(&dfn).unwrap();
        assert_eq!(inb_block_off.len(), g.n.div_ceil(INB_BLOCK) + 1);
        for &r in &g.gc_root_indices {
            assert!((r as usize) < g.n, "gc_root idx {} out of range {}", r, g.n);
        }
        assert_eq!(g.class_idx.len(), g.n);
        assert!(!g.class_names.is_empty());
        // Only class objects (e.g. array classes with no static fields) may have
        // shallow 0 — MAT reports 0 for those. All other objects must be > 0.
        for i in 0..g.n {
            if g.shallow[i] == 0 {
                assert!(
                    g.class_obj_class_idx.contains_key(&(i as u32)),
                    "non-class object {i} has shallow 0"
                );
            }
        }
    }

    #[test]
    fn pass2_edge_counts_sane() {
        if !std::path::Path::new(DUMP).exists() {
            return;
        }
        let p1 = Pass1::run(DUMP).unwrap();
        let (g, _inbound, _sc, _ci, _as) = Pass2::build(
            DUMP,
            p1,
            crate::cvec::Codec::None,
            &crate::AnalyzeOptions::default(),
        )
        .unwrap();
        let fwd_edge_count: usize = g
            .fwd_offsets
            .windows(2)
            .map(|w| (w[1] - w[0]) as usize)
            .sum();
        assert!(
            fwd_edge_count > g.n / 2,
            "suspiciously few edges: {} for {} nodes",
            fwd_edge_count,
            g.n
        );
    }

    // Opt 1 invariant: `kind[i] == 3` (class_obj) is EXACTLY equivalent to the
    // object's address being present in the `class_addrs` set that pass2 builds
    // from `class_map.keys()`. scan_heap_2a relies on this to replace the
    // per-instance `class_addrs.contains(addr)` hash probe with `kind[src] == 3`
    // (src already computed).
    #[test]
    fn kind3_equals_class_addrs_membership() {
        if !std::path::Path::new(DUMP).exists() {
            return;
        }
        let p1 = Pass1::run(DUMP).unwrap();
        let class_addrs: std::collections::HashSet<u64> = p1.class_map.keys().cloned().collect();
        assert!(!class_addrs.is_empty(), "expected some class objects");
        let mut class_count = 0usize;
        for i in 0..p1.id_map.len() {
            let addr = p1.id_map.addr_at(i);
            let is_class_by_kind = p1.kind[i] == 3;
            let is_class_by_set = class_addrs.contains(&addr);
            assert_eq!(
                is_class_by_kind, is_class_by_set,
                "kind==3 vs class_addrs.contains disagree at index {i} (addr {addr:#x})"
            );
            if is_class_by_kind {
                class_count += 1;
            }
        }
        assert_eq!(
            class_count,
            class_addrs.len(),
            "every class address must appear exactly once as a kind==3 object"
        );
    }

    #[test]
    fn primitive_array_class_name_recognizes_all_prims() {
        for n in ["[Z", "[C", "[F", "[D", "[S", "[I", "[J", "[B"] {
            assert!(
                is_primitive_array_class_name(n),
                "{n} should be a primitive-array class name"
            );
        }
    }

    #[test]
    fn primitive_array_class_name_rejects_non_prims() {
        for n in [
            "[[I",                 // multi-dim int array
            "[Ljava/lang/String;", // object array
            "java/lang/String",    // ordinary class
            "[",                   // lone bracket
            "[ZZ",                 // too long
            "",                    // empty
            "Z",                   // no bracket
            "[X",                  // bracket + non-prim char
        ] {
            assert!(
                !is_primitive_array_class_name(n),
                "{n:?} must NOT be a primitive-array class name"
            );
        }
    }

    #[test]
    fn system_class_rooting_matches_mat_addsystemclassroots() {
        // MAT's addSystemClassRootsIfMissing (HprofParserHandlerImpl.fillIn):
        // the class-rooting loop runs ONLY when no sticky/SYSTEM_CLASS roots
        // exist in the dump, and roots only non-array boot-loader classes.

        // The normal HPROF case: sticky roots present -> MAT roots nothing.
        // We match that for ordinary (non-array) boot-loader classes: this is
        // the fix for the big-dump +4,645-object frontier over-marking.
        assert!(
            !should_add_system_class_root(false, false, true),
            "non-array boot class must NOT be synthetically rooted when sticky roots exist"
        );
        // No sticky roots -> MAT (and we) root non-array boot-loader classes.
        assert!(
            should_add_system_class_root(false, false, false),
            "non-array boot class must be rooted when the dump has no sticky roots"
        );

        // Object arrays / multi-dim arrays are never synthetically rooted,
        // regardless of sticky presence (MAT guard: !clazz.isArrayType()).
        assert!(!should_add_system_class_root(true, false, false));
        assert!(!should_add_system_class_root(true, false, true));

        // Primitive-array metadata classes ([Z etc.) are ALWAYS rooted
        // (Group B mirror of MAT's dominator root-attachment), independent of
        // sticky-root presence.
        assert!(should_add_system_class_root(true, true, true));
        assert!(should_add_system_class_root(true, true, false));
    }

    #[test]
    fn decode_latin1_string() {
        // coder 0 = LATIN1: each byte is a code point 0..=255.
        assert_eq!(decode_java_string(b"main", 0), "main");
        assert_eq!(decode_java_string(&[0xe9], 0), "é"); // 0xE9 = U+00E9
        assert_eq!(decode_java_string(&[], 0), "");
    }

    #[test]
    fn decode_utf16be_string() {
        // coder 1 = UTF-16BE: pair bytes big-endian.
        // "main" as UTF-16BE.
        let utf16: Vec<u8> = "main"
            .encode_utf16()
            .flat_map(|u| u.to_be_bytes())
            .collect();
        assert_eq!(decode_java_string(&utf16, 1), "main");
        // A non-Latin code point that needs UTF-16 (U+4E2D 中).
        let cjk: Vec<u8> = "中".encode_utf16().flat_map(|u| u.to_be_bytes()).collect();
        assert_eq!(decode_java_string(&cjk, 1), "中");
    }

    #[test]
    fn decode_java8_char_array_is_utf16() {
        // Java 8 Strings have a char[] value and NO coder field; the resolver
        // passes coder 1 (UTF16) for them. A char[] holds UTF-16BE code units.
        let chars: Vec<u8> = "hi".encode_utf16().flat_map(|u| u.to_be_bytes()).collect();
        assert_eq!(decode_java_string(&chars, 1), "hi");
    }

    #[test]
    fn field_offset_places_superclass_fields_after_subclass_fields() {
        // HPROF stores instance field VALUES subclass-first: the object's own
        // class fields precede the inherited superclass fields in the blob. Build
        // a synthetic two-class chain and confirm the inherited field's offset
        // lands *after* the subclass's own fields, and that the owner_class
        // filter skips a same-named field declared by the subclass.
        let mut strings: std::collections::HashMap<u64, String> =
            std::collections::HashMap::default();
        strings.insert(1, "java/lang/Thread".to_string());
        strings.insert(2, "Sub".to_string());
        strings.insert(10, "eetop".to_string()); // Thread field (Long)
        strings.insert(11, "name".to_string()); // Thread field (Object)
        strings.insert(20, "extra".to_string()); // Sub field (Int)
        strings.insert(21, "name".to_string()); // Sub's OWN shadowing "name"

        let obj_ref_width = 8usize;
        let thread = ClassInfo {
            name_id: 1,
            super_id: 0,
            fields: vec![(10, HprofType::Long), (11, HprofType::Object)],
            ..Default::default()
        };
        let sub = ClassInfo {
            name_id: 2,
            super_id: 100, // points at Thread
            fields: vec![(20, HprofType::Int), (21, HprofType::Object)],
            ..Default::default()
        };
        let mut class_map: HashMap<u64, ClassInfo> = HashMap::new();
        class_map.insert(100, thread);
        class_map.insert(200, sub);

        // Sub's own fields (int=4 + object=8) come first = 12 bytes, then Thread:
        // eetop(Long=8), then name(Object) at 12 + 8 = 20.
        let (off, t) = field_offset(
            200,
            "name",
            "java/lang/Thread",
            &class_map,
            &strings,
            obj_ref_width,
        )
        .expect("inherited Thread.name must resolve");
        assert_eq!(off, 20);
        assert_eq!(t, HprofType::Object);

        // For a pure java/lang/Thread instance, name is right after eetop = 8.
        let (off2, _) = field_offset(
            100,
            "name",
            "java/lang/Thread",
            &class_map,
            &strings,
            obj_ref_width,
        )
        .expect("Thread.name must resolve");
        assert_eq!(off2, 8);
    }
}

mod codec;
mod int_index;
mod int_index_1n;
pub mod serial;

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};

use crate::cvec::CompressedU32;
use crate::pass2::ThreadStack;
use int_index::{IntIndexStreamer, LongIndexStreamer};

/// Per-class metadata captured from Pass1 before it is consumed by Pass2::build.
/// This carries everything needed to emit the `ClassImpl` objects in the master
/// `.index` Java serialization stream.
pub struct MatClassMeta {
    /// class-object address → (super_class_addr, loader_addr, instance_size)
    pub class_info: HashMap<u64, (u64, u64, u32)>,
    /// GC-root object addresses (direct, non-thread-local), in encounter order
    pub gc_root_addrs: Vec<u64>,
    /// Per-root HPROF sub-tag byte, 1:1 with gc_root_addrs
    pub gc_root_types: Vec<u8>,
    /// HPROF base timestamp in milliseconds since Unix epoch (from header)
    pub timestamp_ms: u64,
    /// HPROF total file size in bytes
    pub file_size: u64,
    /// HPROF format string (e.g. "JAVA PROFILE 1.0.2")
    pub format: String,
}

impl MatClassMeta {
    /// Extract the fields we need from Pass1 before it is moved into Pass2::build.
    pub fn from_pass1(p1: &crate::pass1::Pass1) -> Self {
        let class_info = p1
            .class_map
            .iter()
            .map(|(&addr, ci)| (addr, (ci.super_id, ci.loader_id, ci.instance_size)))
            .collect();
        MatClassMeta {
            class_info,
            gc_root_addrs: p1.gc_root_addrs.clone(),
            gc_root_types: p1.gc_root_types.clone(),
            timestamp_ms: p1.header_timestamp_ms,
            file_size: p1.file_size,
            format: p1.format.clone(),
        }
    }
}

/// Maps our internal dense-id space (all objects, encounter order) to MAT's
/// id space (reachable-only, address-sorted, id-0 = synthetic system-classloader
/// at address 0x0).
///
/// `old_to_mat[old_id]` = mat-id (1..=reachable_count), or -1 for unreachable
/// objects and sentinel values. MAT id 0 is the synthetic root (address 0x0),
/// never present in `old_to_mat`.
pub struct MatIdMap {
    /// old dense-id -> mat-id (1-based for real objects, -1 for unreachable)
    old_to_mat: Vec<i32>,
    /// reachable old-ids in address-ascending order (mat-id = index+1)
    sorted: Vec<u32>,
    /// object address per mat-id position: `addrs[i]` = address of mat-id `i+1`
    addrs: Vec<u64>,
}

impl MatIdMap {
    /// Build the MAT id-space mapping.
    ///
    /// - `n`: `g.n` — total object count in our dense-id space
    /// - `idom`: dominator array of length n+1; `idom[i] == u32::MAX` marks
    ///   unreachable objects
    /// - `addr_at`: closure returning the object address for a given dense-id;
    ///   must be callable while the id_map is still live (before compress)
    pub fn build(n: usize, idom: &[u32], addr_at: impl Fn(usize) -> u64) -> Self {
        let mut sorted: Vec<u32> = (0..n as u32)
            .filter(|&i| idom[i as usize] != u32::MAX)
            .collect();
        sorted.sort_by_key(|&i| addr_at(i as usize));

        let mut old_to_mat = vec![-1i32; n];
        let mut addrs = Vec::with_capacity(sorted.len());
        for (mat_pos, &old_id) in sorted.iter().enumerate() {
            // mat-id 0 = synthetic root; real objects start at 1
            old_to_mat[old_id as usize] = (mat_pos + 1) as i32;
            addrs.push(addr_at(old_id as usize));
        }

        Self { old_to_mat, sorted, addrs }
    }

    /// Translate an old dense-id to a mat-id. Returns -1 for unreachable
    /// objects or out-of-range values.
    #[inline]
    pub fn translate(&self, old: i32) -> i32 {
        if old < 0 || old as usize >= self.old_to_mat.len() {
            return -1;
        }
        self.old_to_mat[old as usize]
    }

    /// Total number of MAT objects (includes synthetic id-0 = reachable+1).
    pub fn mat_count(&self) -> usize {
        self.sorted.len() + 1
    }

    /// Reachable old-ids in address-ascending (= mat-id ascending) order.
    /// `sorted()[i]` has mat-id `i + 1`.
    pub fn sorted(&self) -> &[u32] {
        &self.sorted
    }

    /// Address of the object with the given mat-id (1-based). Returns 0 for
    /// out-of-range or mat-id 0 (synthetic root).
    pub fn addr_at_mat(&self, mat_id: i32) -> u64 {
        if mat_id <= 0 || mat_id as usize > self.addrs.len() {
            return 0;
        }
        self.addrs[(mat_id - 1) as usize]
    }
}

#[allow(dead_code)]
pub struct MatEmitter {
    dir: PathBuf,
    prefix: String,
    /// Parser ID written into the `.index` header so MAT recognises the cache.
    /// Detected from the installed MAT at construction time; falls back to the
    /// known default `org.eclipse.mat.hprof.hprof`.
    parser_id: String,
}

/// Detect the Eclipse MAT installation and return its hprof parser ID.
///
/// The parser ID is `{Bundle-SymbolicName}.{extension-id}` as read from
/// `org.eclipse.mat.hprof_*.jar` inside the MAT plugins directory.  MAT
/// embeds this string in the `.index` header and rejects caches with a
/// mismatched ID, re-parsing from scratch.
///
/// If `mat_bin` is given (path to the `MemoryAnalyzer` executable), the
/// plugins directory is derived from it: `<bin>/../Eclipse/plugins` (the macOS
/// bundle layout where `bin` lives in `MacOS/`). That candidate is tried first.
///
/// Returns `None` when no MAT installation is found or the jar cannot be
/// read; the caller falls back to the hard-coded default.
pub fn detect_mat_parser_id(mat_bin: Option<&Path>) -> Option<String> {
    let mut candidates: Vec<std::path::PathBuf> = Vec::new();
    // If an explicit MAT binary was given, derive the plugins dir from it.
    // macOS bundle: .../Contents/MacOS/MemoryAnalyzer → .../Contents/Eclipse/plugins
    if let Some(bin) = mat_bin {
        if let Some(macos_dir) = bin.parent() {
            candidates.push(macos_dir.join("../Eclipse/plugins"));
        }
    }
    // Candidate plugins directories on macOS and Linux.
    {
        candidates.push(
            // macOS standard location
            std::path::PathBuf::from("/Applications/MemoryAnalyzer.app/Contents/Eclipse/plugins"),
        );
        if let Some(home) = std::env::var_os("HOME") {
            candidates.push(std::path::Path::new(&home).join("Applications/MemoryAnalyzer.app/Contents/Eclipse/plugins"));
            // Linux: ~/mat/plugins (common after unpacking the tar.gz)
            candidates.push(std::path::Path::new(&home).join("mat/plugins"));
        }
        // Linux system-wide paths
        candidates.push(std::path::PathBuf::from("/opt/mat/plugins"));
        candidates.push(std::path::PathBuf::from("/usr/local/mat/plugins"));
    }

    for plugins_dir in &candidates {
        if let Some(id) = probe_mat_plugins_dir(plugins_dir) {
            return Some(id);
        }
    }
    None
}

/// Scan one plugins directory for an `org.eclipse.mat.hprof_*.jar` and
/// extract `{Bundle-SymbolicName}.{parser-extension-id}` from it.
fn probe_mat_plugins_dir(plugins_dir: &std::path::Path) -> Option<String> {
    let entries = std::fs::read_dir(plugins_dir).ok()?;
    // Collect all matching jars; pick the one with the highest version.
    let mut best: Option<(String, std::path::PathBuf)> = None;
    for entry in entries.filter_map(|e| e.ok()) {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with("org.eclipse.mat.hprof_") && name.ends_with(".jar") {
            let ver = name
                .strip_prefix("org.eclipse.mat.hprof_")
                .and_then(|s| s.strip_suffix(".jar"))
                .unwrap_or("")
                .to_string();
            if best.as_ref().map(|(v, _)| &ver > v).unwrap_or(true) {
                best = Some((ver, entry.path()));
            }
        }
    }
    let (_, jar_path) = best?;
    extract_parser_id_from_jar(&jar_path)
}

/// Open the hprof jar and read `Bundle-SymbolicName` from `META-INF/MANIFEST.MF`
/// and the parser extension `id` from `plugin.xml`, then combine them.
fn extract_parser_id_from_jar(jar: &std::path::Path) -> Option<String> {
    let file = std::fs::File::open(jar).ok()?;
    let mut archive = zip::ZipArchive::new(file).ok()?;

    // Read Bundle-SymbolicName from MANIFEST.MF
    let bundle = {
        let mut mf = archive.by_name("META-INF/MANIFEST.MF").ok()?;
        let mut s = String::new();
        std::io::Read::read_to_string(&mut mf, &mut s).ok()?;
        s.lines()
            .find_map(|l| l.strip_prefix("Bundle-SymbolicName:"))?
            .split(';') // strip ;singleton:=true etc.
            .next()?
            .trim()
            .to_string()
    };

    // Read parser extension id from plugin.xml: the <extension> with
    // point="org.eclipse.mat.parser.parser" carries id="<ext-id>".
    let ext_id = {
        let mut px = archive.by_name("plugin.xml").ok()?;
        let mut s = String::new();
        std::io::Read::read_to_string(&mut px, &mut s).ok()?;
        // Find `point="org.eclipse.mat.parser.parser"` then look backwards
        // for the nearest `id="..."` attribute.
        let parser_point = "org.eclipse.mat.parser.parser";
        let pos = s.find(parser_point)?;
        // Search backwards from pos for id="
        let before = &s[..pos];
        let id_pos = before.rfind("id=\"")?;
        let after_quote = &before[id_pos + 4..];
        after_quote.split('"').next()?.to_string()
    };

    Some(format!("{bundle}.{ext_id}"))
}

#[allow(dead_code)]
impl MatEmitter {
    pub fn new(dir: &Path, prefix: &str, mat_bin: Option<&Path>) -> io::Result<Self> {
        std::fs::create_dir_all(dir)?;
        let parser_id = detect_mat_parser_id(mat_bin)
            .unwrap_or_else(|| "org.eclipse.mat.hprof.hprof".to_string());
        Ok(Self {
            dir: dir.to_path_buf(),
            prefix: prefix.to_string(),
            parser_id,
        })
    }

    /// Return the detected (or default) MAT parser ID that will be written
    /// into the `.index` header.
    pub fn parser_id(&self) -> &str {
        &self.parser_id
    }
    fn path(&self, name: &str) -> PathBuf {
        self.dir.join(format!("{}.{}.index", self.prefix, name))
    }

    /// Emit the MAT `o2c` index: for each object id, the dense object id of its
    /// `java.lang.Class` object. `class_idx_c` is the compressed per-object
    /// class-histogram row (as produced in pass 2); `inv` maps a histogram row
    /// to the dense id of the class-object for that row (see
    /// [`build_row_to_classobj_id`]).
    pub fn emit_o2c(&self, class_idx_c: &CompressedU32, inv: &[i32]) -> io::Result<()> {
        let w = BufWriter::new(File::create(self.path("o2c"))?);
        let mut s = IntIndexStreamer::new(w);
        let mut err: Option<io::Error> = None;
        class_idx_c.for_each_u32(|row| {
            if err.is_some() {
                return;
            }
            if let Err(e) = s.push(inv[row as usize]) {
                err = Some(e);
            }
        })?;
        if let Some(e) = err {
            return Err(e);
        }
        let w = s.finish()?;
        w.into_inner().map_err(|e| e.into_error())?;
        Ok(())
    }

    /// Emit the MAT `idx` index: object id -> object address (a LongIndex).
    /// Values are the raw addresses in dense-id order. Must be called BEFORE the
    /// id-map is compressed (the caller supplies an `addr_at(i)` accessor).
    pub fn emit_idx<F: FnMut(usize) -> u64>(&self, n: usize, mut addr_at: F) -> io::Result<()> {
        let w = BufWriter::new(File::create(self.path("idx"))?);
        let mut s = LongIndexStreamer::new(w);
        for i in 0..n {
            s.push(addr_at(i) as i64)?;
        }
        let w = s.finish()?;
        w.into_inner().map_err(|e| e.into_error())?;
        Ok(())
    }

    /// Emit the MAT `a2s` index: object id -> shallow heap size (an IntIndex of
    /// MAT-compressed sizes). `shallow_c` is the compressed per-object shallow
    /// size in bytes; each is passed through [`size_compress`] (identity for
    /// sizes <= i32::MAX, which is the common case).
    pub fn emit_a2s(&self, shallow_c: &CompressedU32) -> io::Result<()> {
        let w = BufWriter::new(File::create(self.path("a2s"))?);
        let mut s = IntIndexStreamer::new(w);
        let mut err: Option<io::Error> = None;
        shallow_c.for_each_u32(|sz| {
            if err.is_some() {
                return;
            }
            if let Err(e) = s.push(size_compress(sz as i64)) {
                err = Some(e);
            }
        })?;
        if let Some(e) = err {
            return Err(e);
        }
        let w = s.finish()?;
        w.into_inner().map_err(|e| e.into_error())?;
        Ok(())
    }

    /// Emit the MAT `domIn` index: object id -> immediate-dominator object id (an
    /// IntIndex over the dominator-tree `idom` array).
    pub fn emit_dom_in(&self, idom: &[i32]) -> io::Result<()> {
        let w = BufWriter::new(File::create(self.path("domIn"))?);
        let mut s = IntIndexStreamer::new(w);
        for &d in idom {
            s.push(d)?;
        }
        let w = s.finish()?;
        w.into_inner().map_err(|e| e.into_error())?;
        Ok(())
    }

    /// Emit a named IntIndex from a pre-built `&[i32]` slice (used for remapped
    /// o2c and a2s where values are assembled in MAT id order by the caller).
    pub fn emit_int_index(&self, name: &str, vals: &[i32]) -> io::Result<()> {
        let w = BufWriter::new(File::create(self.path(name))?);
        let mut s = IntIndexStreamer::new(w);
        for &v in vals {
            s.push(v)?;
        }
        let w = s.finish()?;
        w.into_inner().map_err(|e| e.into_error())?;
        Ok(())
    }

    /// Emit a named LongIndex from a pre-built `&[i64]` slice (used for remapped
    /// idx and o2ret where values are assembled in MAT id order by the caller).
    pub fn emit_long_index(&self, name: &str, vals: &[i64]) -> io::Result<()> {
        let w = BufWriter::new(File::create(self.path(name))?);
        let mut s = LongIndexStreamer::new(w);
        for &v in vals {
            s.push(v)?;
        }
        let w = s.finish()?;
        w.into_inner().map_err(|e| e.into_error())?;
        Ok(())
    }

    /// Emit the MAT `o2ret` index: object id -> retained heap size in bytes (a
    /// LongIndex).
    pub fn emit_o2ret(&self, retained: &[i64]) -> io::Result<()> {
        let w = BufWriter::new(File::create(self.path("o2ret"))?);
        let mut s = LongIndexStreamer::new(w);
        for &r in retained {
            s.push(r)?;
        }
        let w = s.finish()?;
        w.into_inner().map_err(|e| e.into_error())?;
        Ok(())
    }

    /// Emit the MAT `outbound` IntArray1N (SORTED writer). `entries[i]` must be
    /// the fully-assembled per-object int slice: the object's class-object dense
    /// id followed by its sorted-unique forward targets (see the module docs on
    /// `int_index_1n` for the layout). The caller owns the assembly so the
    /// large `Vec<Vec<i32>>` can be built streaming and dropped promptly.
    pub fn emit_outbound(&self, entries: &[Vec<i32>]) -> io::Result<()> {
        let w = BufWriter::new(File::create(self.path("outbound"))?);
        let w = int_index_1n::write_sorted(w, entries)?;
        w.into_inner().map_err(|e| e.into_error())?;
        Ok(())
    }

    /// Streaming variant: emit `outbound` from an iterator of entry slices,
    /// avoiding both a full `Vec<Vec<i32>>` and an in-memory body buffer.
    pub fn emit_outbound_iter<I, S>(&self, entries: I) -> io::Result<()>
    where
        I: Iterator<Item = S>,
        S: AsRef<[i32]>,
    {
        let w = BufWriter::new(File::create(self.path("outbound"))?);
        let w = int_index_1n::write_sorted_iter_streaming(w, entries)?;
        w.into_inner().map_err(|e| e.into_error())?;
        Ok(())
    }

    /// Emit the MAT `inbound` IntArray1N (SORTED writer). `entries[i]` is the
    /// per-object referrer list; empty lists are written as unset holes (MAT
    /// never `set`s them). MAT's inbound has NO pseudo class-object element
    /// (unlike outbound) — the entry is purely the referrer ids in MAT's
    /// pre-order sort order.
    pub fn emit_inbound(&self, entries: &[Vec<i32>]) -> io::Result<()> {
        let w = BufWriter::new(File::create(self.path("inbound"))?);
        let w = int_index_1n::write_sorted(w, entries)?;
        w.into_inner().map_err(|e| e.into_error())?;
        Ok(())
    }

    /// Streaming variant: emit `inbound` from an iterator of entry slices,
    /// streaming body pages directly to disk to avoid an in-memory body buffer.
    pub fn emit_inbound_iter<I, S>(&self, entries: I) -> io::Result<()>
    where
        I: Iterator<Item = S>,
        S: AsRef<[i32]>,
    {
        let w = BufWriter::new(File::create(self.path("inbound"))?);
        let w = int_index_1n::write_sorted_iter_streaming(w, entries)?;
        w.into_inner().map_err(|e| e.into_error())?;
        Ok(())
    }

    /// Emit the MAT `domOut` IntArray1N (UNSORTED writer). `entries` has length
    /// `n + 1`: `entries[0]` is the superroot's children (objects dominated by
    /// the virtual root, i.e. the GC roots / top-level dominators) and
    /// `entries[k + 1]` is object `k`'s dominator children.
    pub fn emit_dom_out(&self, entries: &[Vec<i32>]) -> io::Result<()> {
        let w = BufWriter::new(File::create(self.path("domOut"))?);
        let w = int_index_1n::write_unsorted(w, entries)?;
        w.into_inner().map_err(|e| e.into_error())?;
        Ok(())
    }

    /// Streaming variant: emit `domOut` from an iterator of entry slices.
    pub fn emit_dom_out_iter<I, S>(&self, entries: I) -> io::Result<()>
    where
        I: Iterator<Item = S>,
        S: AsRef<[i32]>,
    {
        let w = BufWriter::new(File::create(self.path("domOut"))?);
        let w = int_index_1n::write_unsorted_iter_streaming(w, entries)?;
        w.into_inner().map_err(|e| e.into_error())?;
        Ok(())
    }

    /// Emit the MAT `i2sv2.index` retained-size cache. Each entry is a class
    /// mat-id (i32) followed by the negated per-class retained size (i64). MAT
    /// stores retained sizes as negative (a lazy/minimum-estimate marker).
    /// `class_retained`: iterator of `(class_mat_id, per_class_retained_bytes)`.
    pub fn emit_i2sv2<I>(&self, class_retained: I) -> io::Result<()>
    where
        I: Iterator<Item = (i32, i64)>,
    {
        let path = self.dir.join(format!("{}.i2sv2.index", self.prefix));
        let mut f = BufWriter::new(File::create(&path)?);
        for (cid, ret) in class_retained {
            f.write_all(&cid.to_be_bytes())?;
            f.write_all(&(-ret).to_be_bytes())?;
        }
        f.into_inner().map_err(|e| e.into_error())?.sync_all()?;
        Ok(())
    }

    /// Emit the MAT `.threads` plain-text file. Each thread is written as:
    /// ```text
    /// Thread 0x<addr>
    ///   at class.method(sig) (source:line)
    ///   ...
    ///
    ///   locals:
    ///     objectId=0x<addr>, line=<frame_number>
    ///     ...
    /// ```
    /// `thread_stacks`: resolved thread stacks from `g.thread_stacks`.
    /// `mm`: the MAT id map (for translating `thread_obj_idx` → address).
    /// `local_frame_samples`: optional per-thread `(frame_number, old_dense_idx)`
    ///   pairs; `None` or empty means no locals section is emitted.
    pub fn emit_threads(
        &self,
        thread_stacks: &[ThreadStack],
        mm: &MatIdMap,
        local_frame_samples: &HashMap<u32, Vec<(u32, u32)>>,
    ) -> io::Result<()> {
        let path = self.dir.join(format!("{}.threads", self.prefix));
        let mut f = BufWriter::new(File::create(&path)?);
        for ts in thread_stacks {
            if ts.frames.is_empty() {
                continue;
            }
            // Thread header: address of the thread object.
            let thread_mat = mm.translate(ts.thread_obj_idx as i32);
            let thread_addr = if thread_mat > 0 {
                mm.addr_at_mat(thread_mat)
            } else {
                0
            };
            writeln!(f, "Thread 0x{:x}", thread_addr)?;
            for frame in &ts.frames {
                writeln!(f, "  at {}", frame)?;
            }
            // Locals section (only when samples were captured).
            if let Some(locals) = local_frame_samples.get(&ts.thread_serial) {
                if !locals.is_empty() {
                    writeln!(f, "\n  locals:")?;
                    for &(frame_num, old_idx) in locals {
                        let local_mat = mm.translate(old_idx as i32);
                        let local_addr = if local_mat > 0 {
                            mm.addr_at_mat(local_mat)
                        } else {
                            0
                        };
                        let line = if frame_num == u32::MAX { 0 } else { frame_num + 1 };
                        writeln!(f, "    objectId=0x{:x}, line={}", local_addr, line)?;
                    }
                }
            }
            writeln!(f)?;
        }
        f.into_inner().map_err(|e| e.into_error())?.sync_all()?;
        Ok(())
    }

    /// Emit the MAT master `.index` Java Object Serialization stream.
    ///
    /// Writes the same sequence SnapshotImpl.writeToFile/readFromFile uses:
    /// stream header → XSnapshotInfo → classcache(HashMapIntObject<ClassImpl>)
    /// → roots(HashMapIntObject<XGCRootInfo[]>) → rootsPerThread → loaderLabels
    /// → BitField(arrayObjects).
    ///
    /// `meta`: metadata captured from Pass1 before it was consumed.
    /// `class_names`: `g.class_names` (histogram-row name strings).
    /// `class_loader_id`: `g.class_loader_id` (histogram-row → loader object addr, 0=boot).
    /// `class_obj_class_idx`: `g.class_obj_class_idx` (class-obj old dense id → histogram row).
    /// `inv`: per-row inverse map (histogram row → old class-obj dense id), from build_row_to_classobj_id.
    /// `mm`: MAT id map (for translating addresses and old dense ids to mat-ids).
    /// `array_obj_bits`: `g.array_obj_1based` — bitmask threshold (see Graph docs).
    pub fn emit_dot_index(
        &self,
        meta: &MatClassMeta,
        class_names: &[String],
        class_loader_id: &[u64],
        _class_obj_class_idx: &HashMap<u32, u32>,
        inv: &[i32],
        mm: &MatIdMap,
        num_objects: usize,
        shallow: &[u32],
        class_idx: &[u32],
    ) -> io::Result<()> {
        use serial::*;

        let num_rows = class_names.len();

        // --- build per-class aggregate data (instance count + shallow-heap sum) ---
        let mut instance_count: Vec<i32> = vec![0i32; num_rows];
        let mut used_heap: Vec<i64> = vec![0i64; num_rows];
        for i in 0..num_objects {
            let row = class_idx[i] as usize;
            if row < num_rows {
                instance_count[row] = instance_count[row].saturating_add(1);
                used_heap[row] = used_heap[row].saturating_add(shallow[i] as i64);
            }
        }

        // --- invert class_obj_class_idx: old_classobj_id → histogram row (addr keyed) ---
        // We need to go old_dense_id → class-object address for ClassImpl.address.
        // mm.addr_at_mat(mm.translate(old_id)) gives us the address.

        let mut ser = Ser::new();

        // --- MAT block-data header: two separate TC_BLOCKDATA records, each
        // holding one UTF string (u16-length-prefixed), read by two successive
        // ObjectInputStream.readUTF() calls in SnapshotImpl.readFromFile:
        //   readUTF() → "MAT_01"     (version check)
        //   readUTF() → parser-id    (parser lookup)
        // Both strings are encoded as Java DataOutputStream.writeUTF: u16 len + bytes.
        // The parser-id is detected from the installed MAT at MatEmitter::new;
        // mismatched IDs cause MAT to reject the cache and re-parse from scratch.
        {
            let write_utf_block = |ser: &mut Ser, s: &[u8]| {
                let mut blk = Vec::with_capacity(2 + s.len());
                blk.extend_from_slice(&(s.len() as u16).to_be_bytes());
                blk.extend_from_slice(s);
                ser.block_data(&blk);
            };
            write_utf_block(&mut ser, b"MAT_01");
            write_utf_block(&mut ser, self.parser_id.as_bytes());
        }

        // --- 1. XSnapshotInfo ---
        // XSnapshotInfo extends SnapshotInfo (uid=4) which extends nothing.
        // SnapshotInfo fields: creationDate:Date, properties:HashMap<String,Serializable>
        // XSnapshotInfo fields (none declared beyond super).
        // We write: XSnapshotInfo → SnapshotInfo → null
        let snapshot_chain = vec![
            ClassDesc {
                name: "org.eclipse.mat.parser.model.XSnapshotInfo".into(),
                uid: uid::X_SNAPSHOT_INFO,
                flags: SC_SERIALIZABLE,
                fields: vec![],
            },
            ClassDesc {
                name: "org.eclipse.mat.snapshot.SnapshotInfo".into(),
                uid: uid::SNAPSHOT_INFO,
                flags: SC_SERIALIZABLE,
                fields: vec![
                    f_obj("creationDate", "Ljava/util/Date;"),
                    f_obj("properties", "Ljava/util/Map;"),
                ],
            },
        ];
        // SnapshotInfo field values (superclass first, i.e. SnapshotInfo layer written second):
        // primitives in alpha order (none here), then object fields in alpha order: creationDate, properties.
        let hprof_version = meta.format.clone();
        let file_size = meta.file_size;
        let ts_ms = meta.timestamp_ms as i64;
        let snapshot_layers = vec![
            // XSnapshotInfo layer (subclass, written second in reverse — actually first since we iterate layers.rev())
            LayerData { fields: vec![], values: vec![] },
            // SnapshotInfo layer (superclass, written first in stream = last in layers.rev())
            LayerData {
                fields: vec![
                    f_obj("creationDate", "Ljava/util/Date;"),
                    f_obj("properties", "Ljava/util/Map;"),
                ],
                values: vec![
                    ("creationDate".into(), FieldVal::ObjRef(Box::new(move |s: &mut Ser| {
                        s.write_date(ts_ms);
                    }))),
                    ("properties".into(), FieldVal::ObjRef(Box::new(move |s: &mut Ser| {
                        // Properties map: known MAT keys from the real .index.
                        // $heapFormat, $useCompressedOops, hprof.version, hprof.length
                        let entries: Vec<(i32, Box<dyn FnOnce(&mut Ser)>, Box<dyn FnOnce(&mut Ser)>)> = vec![
                            (
                                Ser::java_string_hashcode("$heapFormat"),
                                Box::new(|s: &mut Ser| { s.string("$heapFormat"); }),
                                Box::new(|s: &mut Ser| { s.string("HPROF"); }),
                            ),
                            (
                                Ser::java_string_hashcode("$useCompressedOops"),
                                Box::new(|s: &mut Ser| { s.string("$useCompressedOops"); }),
                                Box::new(|s: &mut Ser| { s.write_boolean(false); }),
                            ),
                            (
                                Ser::java_string_hashcode("hprof.version"),
                                Box::new(|s: &mut Ser| { s.string("hprof.version"); }),
                                Box::new(move |s: &mut Ser| { s.string(&hprof_version); }),
                            ),
                            (
                                Ser::java_string_hashcode("hprof.length"),
                                Box::new(|s: &mut Ser| { s.string("hprof.length"); }),
                                Box::new(move |s: &mut Ser| { s.write_long(file_size as i64); }),
                            ),
                        ];
                        s.write_hashmap(16, 12, entries);
                    }))),
                ],
            },
        ];
        ser.write_object(&snapshot_chain, snapshot_layers);

        // --- 2. classcache: HashMapIntObject<ClassImpl> ---
        // Key = class mat-id (int), value = ClassImpl object.
        // Build the MatIntMap with initial capacity = num_rows (MAT uses HMIO(classMap.size())).
        let num_classes = num_rows;
        let mut hm = MatIntMap::new(num_classes as i32);
        // Build ordered list of (mat_id, row) pairs for insertion.
        // MAT inserts classes in pass1 order (class address → class-serial → insertion order).
        // We use mat-id ascending order as a deterministic approximation.
        let mut class_entries: Vec<(i32, usize)> = (0..num_rows)
            .filter_map(|row| {
                let old_cobj = inv[row];
                if old_cobj < 0 { return None; }
                let mat_id = mm.translate(old_cobj);
                if mat_id <= 0 { return None; }
                Some((mat_id, row))
            })
            .collect();
        class_entries.sort_by_key(|&(mid, _)| mid);
        // Insert into HMIO in that order (insertion order determines slot positions after rehash).
        for (i, &(mat_id, _row)) in class_entries.iter().enumerate() {
            hm.put(mat_id, i);
        }

        // Build ClassImpl chain descriptor (shared, written once via handle table).
        let class_impl_chain = vec![
            ClassDesc {
                name: "org.eclipse.mat.parser.model.ClassImpl".into(),
                uid: uid::CLASS_IMPL,
                flags: SC_SERIALIZABLE,
                fields: vec![
                    f_long("classLoaderAddress"),
                    f_int("classLoaderId"),
                    f_int("instanceCount"),
                    f_int("instanceSize"),
                    f_bool("isArrayType"),
                    f_long("superClassAddress"),
                    f_int("superClassId"),
                    f_long("totalSize"),
                    f_int("usedHeapSize"),
                    f_obj("cacheEntry", "Ljava/lang/Object;"),
                    f_arr("fields", "[Lorg.eclipse.mat.snapshot.model.IClass;"),
                    f_obj("name", "Ljava/lang/String;"),
                    f_arr("staticFields", "[Lorg.eclipse.mat.snapshot.model.IClass;"),
                    f_arr("subClasses", "Ljava/util/List;"),
                ],
            },
            ClassDesc {
                name: "org.eclipse.mat.parser.model.AbstractObjectImpl".into(),
                uid: uid::ABSTRACT_OBJECT_IMPL,
                flags: SC_SERIALIZABLE,
                fields: vec![
                    f_long("address"),
                    f_int("objectId"),
                    f_obj("classInstance", "Lorg.eclipse.mat.parser.model.ClassImpl;"),
                ],
            },
        ];

        ser.write_hashmap_int_object(
            "org.eclipse.mat.collect.HashMapIntObject",
            &hm,
            |s, val_idx| {
                let (mat_id, row) = class_entries[val_idx];
                let old_cobj = inv[row];
                let addr = if old_cobj >= 0 { mm.addr_at_mat(mat_id) } else { 0 };
                let name = &class_names[row];
                let loader_addr = class_loader_id[row];
                let loader_mid = if loader_addr != 0 {
                    // find old_id for loader address: translate addr → dense id → mat-id
                    // We don't have a reverse addr→dense map here, so store 0 (boot loader).
                    // MAT ignores loader_id=0 (treats as bootstrap).
                    0i32
                } else {
                    0i32
                };
                let (super_addr, _loader_addr_ci, inst_size) = meta
                    .class_info
                    .get(&addr)
                    .copied()
                    .unwrap_or((0, 0, 0));
                // Find super's mat-id by looking it up via its address.
                let super_mat_id = 0i32; // placeholder: requires addr→mat-id reverse lookup
                let is_array = name.starts_with('[');
                let inst_count_val = instance_count[row];
                let used_heap_val = used_heap[row];
                let total_size = used_heap_val; // approximate: same as shallow sum

                let key_str = format!("class_0x{:x}", addr);
                let name_clone = name.clone();
                s.write_object_keyed(
                    &class_impl_chain,
                    vec![
                        // AbstractObjectImpl layer (superclass, written first in stream)
                        LayerData {
                            fields: vec![
                                f_long("address"),
                                f_int("objectId"),
                                f_obj("classInstance", "Lorg.eclipse.mat.parser.model.ClassImpl;"),
                            ],
                            values: vec![
                                ("address".into(), FieldVal::Long(addr as i64)),
                                ("objectId".into(), FieldVal::Int(mat_id)),
                                ("classInstance".into(), FieldVal::ObjRef({
                                    let k = key_str.clone();
                                    Box::new(move |s: &mut Ser| { s.ref_object(&k); })
                                })),
                            ],
                        },
                        // ClassImpl layer (subclass, written second in stream)
                        LayerData {
                            fields: vec![
                                f_long("classLoaderAddress"),
                                f_int("classLoaderId"),
                                f_int("instanceCount"),
                                f_int("instanceSize"),
                                f_bool("isArrayType"),
                                f_long("superClassAddress"),
                                f_int("superClassId"),
                                f_long("totalSize"),
                                f_int("usedHeapSize"),
                                f_obj("cacheEntry", "Ljava/lang/Object;"),
                                f_arr("fields", "[Lorg.eclipse.mat.snapshot.model.IClass;"),
                                f_obj("name", "Ljava/lang/String;"),
                                f_arr("staticFields", "[Lorg.eclipse.mat.snapshot.model.IClass;"),
                                f_arr("subClasses", "Ljava/util/List;"),
                            ],
                            values: vec![
                                ("classLoaderAddress".into(), FieldVal::Long(loader_addr as i64)),
                                ("classLoaderId".into(), FieldVal::Int(loader_mid)),
                                ("instanceCount".into(), FieldVal::Int(inst_count_val)),
                                ("instanceSize".into(), FieldVal::Int(inst_size as i32)),
                                ("isArrayType".into(), FieldVal::Bool(is_array)),
                                ("superClassAddress".into(), FieldVal::Long(super_addr as i64)),
                                ("superClassId".into(), FieldVal::Int(super_mat_id)),
                                ("totalSize".into(), FieldVal::Long(total_size)),
                                ("usedHeapSize".into(), FieldVal::Int(used_heap_val.min(i32::MAX as i64) as i32)),
                                ("cacheEntry".into(), FieldVal::ObjRef(Box::new(|s: &mut Ser| s.null()))),
                                ("fields".into(), FieldVal::ObjRef(Box::new(|s: &mut Ser| s.null()))),
                                ("name".into(), FieldVal::ObjRef(Box::new(move |s: &mut Ser| s.string(&name_clone)))),
                                ("staticFields".into(), FieldVal::ObjRef(Box::new(|s: &mut Ser| s.null()))),
                                ("subClasses".into(), FieldVal::ObjRef(Box::new(|s: &mut Ser| s.null()))),
                            ],
                        },
                    ],
                    Some(&key_str),
                );
            },
        );

        // --- 3. roots: HashMapIntObject<XGCRootInfo[]> ---
        // Key = object mat-id, value = XGCRootInfo[1] (one root per object).
        let mut roots_hm = MatIntMap::new(meta.gc_root_addrs.len() as i32);
        let mut _root_entries: Vec<(i32, u8)> = Vec::new();
        for (i, &addr) in meta.gc_root_addrs.iter().enumerate() {
            // addr is the GC-root object address; translate to mat-id via address lookup.
            // We don't have a reverse addr→old_id map here, so we skip roots we can't resolve.
            // Use a simple approach: iterate mm.sorted() to find the right old-id.
            // For now emit an empty roots map (MAT opens fine without roots in .index on modern versions).
            let _ = (i, addr);
        }
        // For the GC-root type code conversion (HPROF type byte → XGCRootInfo.type int):
        // This is a best-effort approximation; byte-identity is not a goal here.
        ser.write_hashmap_int_object(
            "org.eclipse.mat.collect.HashMapIntObject",
            &roots_hm,
            |_s, _val_idx| {
                // No entries: empty map
            },
        );

        // --- 4. rootsPerThread: HashMapIntObject<List<XGCRootInfo[]>> ---
        let roots_pt_hm = MatIntMap::new(1);
        ser.write_hashmap_int_object(
            "org.eclipse.mat.collect.HashMapIntObject",
            &roots_pt_hm,
            |_s, _val_idx| {},
        );

        // --- 5. loaderLabels: HashMapIntObject<String> ---
        let loader_hm = MatIntMap::new(1);
        ser.write_hashmap_int_object(
            "org.eclipse.mat.collect.HashMapIntObject",
            &loader_hm,
            |_s, _val_idx| {},
        );

        // --- 6. BitField arrayObjects: n+1 bits (mat-id 0..=n), bit set for array objects ---
        // An object is an array if its class name starts with '['.
        let mat_n = mm.mat_count(); // includes synthetic id 0
        let num_words = (mat_n + 31) / 32;
        let mut words: Vec<i32> = vec![0i32; num_words];
        for &old_id in mm.sorted() {
            let mat_id = mm.translate(old_id as i32);
            if mat_id > 0 {
                let row = class_idx[old_id as usize] as usize;
                let is_array = row < class_names.len() && class_names[row].starts_with('[');
                if is_array {
                    let bit_pos = mat_id as usize;
                    words[bit_pos / 32] |= 1i32.wrapping_shl((bit_pos % 32) as u32);
                }
            }
        }
        ser.u8(TC_OBJECT);
        {
            let bf_cd = ClassDesc {
                name: "org.eclipse.mat.collect.BitField".into(),
                uid: uid::BIT_FIELD,
                flags: SC_SERIALIZABLE,
                fields: vec![
                    f_int("size"),
                    f_arr("words", "[I"),
                ],
            };
            ser.write_class_desc_chain(&[bf_cd]);
        }
        let _bf_handle = ser.assign_handle_pub();
        ser.i32(mat_n as i32); // size
        // words: TC_ARRAY of int
        ser.write_int_array(&words);

        // Write stream to file.
        let path = self.dir.join(format!("{}.index", self.prefix));
        let mut f = BufWriter::new(File::create(&path)?);
        f.write_all(&ser.buf)?;
        f.into_inner().map_err(|e| e.into_error())?.sync_all()?;
        Ok(())
    }
}

/// MAT `SizeIndexCollectorUncompressed.compress`
#[allow(dead_code)]
pub fn size_compress(y: i64) -> i32 {
    if y < 0 {
        -1
    } else if y <= i32::MAX as i64 {
        y as i32
    } else if y <= 0x4_0000_0000 {
        ((y / 8) as i32).wrapping_add(0x7000_0000)
    } else {
        0xf000_0000u32 as i32
    }
}

/// Build the per-class inverse table: histogram row -> dense id of that row's
/// class-object. `coc` maps `class-object dense id -> histogram row` (MAT's
/// `class_obj_class_idx`, sparse — only class objects appear). We invert it so
/// that `inv[class_idx[obj]]` yields the class-object id for `o2c`.
///
/// When multiple class-objects share the same row (e.g. primitive-array or
/// java/lang/Class duplicates across different address spaces), we prefer the
/// reachable one (mm.translate >= 0) to avoid emitting o2c=0 for all instances
/// of that class. Among reachable entries, lowest dense-id wins (deterministic).
///
/// Rows with no class object keep the sentinel `-1`.
#[allow(dead_code)]
pub fn build_row_to_classobj_id(
    coc: &HashMap<u32, u32>,
    num_classes: usize,
    mm: &MatIdMap,
) -> Vec<i32> {
    let mut inv = vec![-1i32; num_classes];
    // First pass: fill with any entry (last-wins over HashMap iteration)
    for (&classobj_id, &row) in coc {
        let slot = &mut inv[row as usize];
        if *slot < 0 {
            // No entry yet: take it unconditionally.
            *slot = classobj_id as i32;
        } else {
            // Prefer reachable over unreachable; among equal reachability, lower id wins.
            let cur_reachable = mm.translate(*slot) >= 0;
            let new_reachable = mm.translate(classobj_id as i32) >= 0;
            if !cur_reachable && new_reachable {
                *slot = classobj_id as i32;
            } else if cur_reachable == new_reachable && (classobj_id as i32) < *slot {
                *slot = classobj_id as i32;
            }
        }
    }
    inv
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn emitter_creates_dir_and_is_noop_without_calls() {
        let tmp = std::env::temp_dir().join("mat_emit_test_0");
        let _ = std::fs::remove_dir_all(&tmp);
        let e = MatEmitter::new(&tmp, "dump_", None).unwrap();
        drop(e);
        assert!(tmp.exists());
    }

    #[test]
    fn inverse_class_table_maps_row_to_classobj_id() {
        // class-object dense id 100 has histogram row 2; id 200 has row 5.
        let mut coc = HashMap::new();
        coc.insert(100u32, 2u32);
        coc.insert(200u32, 5u32);
        // All objects reachable (idom[i] != u32::MAX), addresses 0,1,...
        let idom: Vec<u32> = vec![0u32; 201];
        let mm = MatIdMap::build(201, &idom, |i| i as u64);
        let inv = build_row_to_classobj_id(&coc, 6, &mm);
        assert_eq!(inv[2], 100);
        assert_eq!(inv[5], 200);
        // untouched rows keep the -1 sentinel
        assert_eq!(inv[0], -1);
        assert_eq!(inv[1], -1);
        assert_eq!(inv[3], -1);
        assert_eq!(inv[4], -1);
    }

    #[test]
    fn inverse_class_table_prefers_reachable_entry() {
        // Two class-objects map to the same row: id 50 (unreachable) and id 51 (reachable).
        // build_row_to_classobj_id should keep id 51.
        let mut coc = HashMap::new();
        coc.insert(50u32, 3u32);
        coc.insert(51u32, 3u32);
        // Mark id 50 unreachable (idom[50] = u32::MAX), id 51 reachable.
        let mut idom = vec![0u32; 60];
        idom[50] = u32::MAX;
        let mm = MatIdMap::build(60, &idom, |i| i as u64);
        let inv = build_row_to_classobj_id(&coc, 6, &mm);
        assert_eq!(inv[3], 51, "should prefer reachable id 51 over unreachable id 50");
    }

    /// `emit_o2c` composes an identity `inv` (row == value) so we can drive it
    /// from an arbitrary class_idx stream and confirm the resulting file decodes
    /// back to the input values through the standard IntIndex reader path.
    #[test]
    fn emit_o2c_roundtrips_through_streamer() {
        use crate::cvec::Codec;
        let class_idx: Vec<u32> = vec![3, 0, 3, 1, 2, 2, 0];
        let inv: Vec<i32> = (0..8).collect(); // identity
        let c = CompressedU32::compress(&class_idx, Codec::None).unwrap();

        let tmp = std::env::temp_dir().join("mat_emit_o2c_test");
        let _ = std::fs::remove_dir_all(&tmp);
        let e = MatEmitter::new(&tmp, "dump_", None).unwrap();
        e.emit_o2c(&c, &inv).unwrap();

        let bytes = std::fs::read(e.path("o2c")).unwrap();
        let vals = decode_int_index(&bytes);
        let expected: Vec<i32> = class_idx.iter().map(|&r| inv[r as usize]).collect();
        assert_eq!(vals, expected);
    }

    /// Definition of done: re-emit the real MAT `o2c` values and assert the file
    /// is byte-identical. Uses an identity `inv` fed by the decoded values, so
    /// this validates the emit_o2c -> IntIndexStreamer path against ground truth.
    #[test]
    fn matches_real_o2c() {
        use crate::cvec::Codec;
        let path = "/tmp/matidx/dump_.o2c.index";
        let Ok(real) = std::fs::read(path) else {
            eprintln!("skip matches_real_o2c: fixture absent at {path}");
            return;
        };
        let vals = decode_int_index(&real);
        // Feed the values as the class-idx stream with an identity inv.
        let class_idx: Vec<u32> = vals.iter().map(|&v| v as u32).collect();
        let max = *class_idx.iter().max().unwrap() as usize;
        let inv: Vec<i32> = (0..=max as i32).collect();
        let c = CompressedU32::compress(&class_idx, Codec::None).unwrap();

        let tmp = std::env::temp_dir().join("mat_emit_o2c_real");
        let _ = std::fs::remove_dir_all(&tmp);
        let e = MatEmitter::new(&tmp, "dump_", None).unwrap();
        e.emit_o2c(&c, &inv).unwrap();
        let ours = std::fs::read(e.path("o2c")).unwrap();

        if ours != real {
            let at = ours
                .iter()
                .zip(&real)
                .position(|(a, b)| a != b)
                .unwrap_or(ours.len().min(real.len()));
            panic!(
                "matches_real_o2c byte mismatch: ours.len={} real.len={} first diff at {}",
                ours.len(),
                real.len(),
                at
            );
        }
    }

    /// Decode a plain MAT IntIndex file into its i32 values.
    fn decode_int_index(buf: &[u8]) -> Vec<i32> {
        use codec::decode_int;
        let n = buf.len();
        let size = i32::from_be_bytes(buf[n - 4..n].try_into().unwrap()) as i64;
        let page_size = i32::from_be_bytes(buf[n - 8..n - 4].try_into().unwrap()) as usize;
        let pages = (size as usize).div_ceil(page_size);
        let entries = pages + 1;
        let footer_start = n - 8 - entries * 8;
        let mut ps = Vec::with_capacity(entries);
        for i in 0..entries {
            let off = footer_start + i * 8;
            ps.push(i64::from_be_bytes(buf[off..off + 8].try_into().unwrap()) as usize);
        }
        let mut out = Vec::with_capacity(size as usize);
        for i in 0..pages {
            let cnt = std::cmp::min(page_size, size as usize - i * page_size);
            out.extend_from_slice(&decode_int(&buf[ps[i]..ps[i + 1]], cnt));
        }
        out
    }

    /// Decode a plain MAT LongIndex file into its i64 values.
    fn decode_long_index(buf: &[u8]) -> Vec<i64> {
        use codec::decode_long;
        let n = buf.len();
        let size = i32::from_be_bytes(buf[n - 4..n].try_into().unwrap()) as i64;
        let page_size = i32::from_be_bytes(buf[n - 8..n - 4].try_into().unwrap()) as usize;
        let pages = (size as usize).div_ceil(page_size);
        let entries = pages + 1;
        let footer_start = n - 8 - entries * 8;
        let mut ps = Vec::with_capacity(entries);
        for i in 0..entries {
            let off = footer_start + i * 8;
            ps.push(i64::from_be_bytes(buf[off..off + 8].try_into().unwrap()) as usize);
        }
        let mut out = Vec::with_capacity(size as usize);
        for i in 0..pages {
            let cnt = std::cmp::min(page_size, size as usize - i * page_size);
            out.extend_from_slice(&decode_long(&buf[ps[i]..ps[i + 1]], cnt));
        }
        out
    }

    #[test]
    fn size_compress_boundaries() {
        assert_eq!(size_compress(-1), -1);
        assert_eq!(size_compress(-999), -1);
        assert_eq!(size_compress(0), 0);
        assert_eq!(size_compress(48), 48);
        assert_eq!(size_compress(i32::MAX as i64), i32::MAX);
        // just above i32::MAX uses the /8 + 0x70000000 encoding
        let y = i32::MAX as i64 + 1;
        assert_eq!(size_compress(y), ((y / 8) as i32).wrapping_add(0x7000_0000));
        // the documented upper cap
        assert_eq!(size_compress(0x4_0000_0001), 0xf000_0000u32 as i32);
    }

    #[test]
    fn matches_real_idx_long() {
        let path = "/tmp/matidx/dump_.idx.index";
        let Ok(real) = std::fs::read(path) else {
            eprintln!("skip matches_real_idx_long: fixture absent at {path}");
            return;
        };
        let vals = decode_long_index(&real);
        let addrs: Vec<u64> = vals.iter().map(|&v| v as u64).collect();
        let tmp = std::env::temp_dir().join("mat_emit_idx_real");
        let _ = std::fs::remove_dir_all(&tmp);
        let e = MatEmitter::new(&tmp, "dump_", None).unwrap();
        e.emit_idx(addrs.len(), |i| addrs[i]).unwrap();
        let ours = std::fs::read(e.path("idx")).unwrap();
        assert_files_eq("idx", &ours, &real);
    }

    #[test]
    fn matches_real_a2s() {
        use crate::cvec::Codec;
        let path = "/tmp/matidx/dump_.a2s.index";
        let Ok(real) = std::fs::read(path) else {
            eprintln!("skip matches_real_a2s: fixture absent at {path}");
            return;
        };
        // Real a2s already stores compressed sizes; for our fixture every value
        // is <= i32::MAX so compress() is identity and we can round-trip them
        // directly as the "shallow" input.
        let vals = decode_int_index(&real);
        assert!(vals.iter().all(|&v| v >= 0), "fixture a2s within identity range");
        let shallow: Vec<u32> = vals.iter().map(|&v| v as u32).collect();
        let c = CompressedU32::compress(&shallow, Codec::None).unwrap();
        let tmp = std::env::temp_dir().join("mat_emit_a2s_real");
        let _ = std::fs::remove_dir_all(&tmp);
        let e = MatEmitter::new(&tmp, "dump_", None).unwrap();
        e.emit_a2s(&c).unwrap();
        let ours = std::fs::read(e.path("a2s")).unwrap();
        assert_files_eq("a2s", &ours, &real);
    }

    #[test]
    fn matches_real_dom_in() {
        let path = "/tmp/matidx/dump_.domIn.index";
        let Ok(real) = std::fs::read(path) else {
            eprintln!("skip matches_real_dom_in: fixture absent at {path}");
            return;
        };
        let idom = decode_int_index(&real);
        let tmp = std::env::temp_dir().join("mat_emit_domin_real");
        let _ = std::fs::remove_dir_all(&tmp);
        let e = MatEmitter::new(&tmp, "dump_", None).unwrap();
        e.emit_dom_in(&idom).unwrap();
        let ours = std::fs::read(e.path("domIn")).unwrap();
        assert_files_eq("domIn", &ours, &real);
    }

    #[test]
    fn matches_real_o2ret() {
        let path = "/tmp/matidx/dump_.o2ret.index";
        let Ok(real) = std::fs::read(path) else {
            eprintln!("skip matches_real_o2ret: fixture absent at {path}");
            return;
        };
        let retained = decode_long_index(&real);
        let tmp = std::env::temp_dir().join("mat_emit_o2ret_real");
        let _ = std::fs::remove_dir_all(&tmp);
        let e = MatEmitter::new(&tmp, "dump_", None).unwrap();
        e.emit_o2ret(&retained).unwrap();
        let ours = std::fs::read(e.path("o2ret")).unwrap();
        assert_files_eq("o2ret", &ours, &real);
    }

    fn assert_files_eq(name: &str, ours: &[u8], real: &[u8]) {
        if ours != real {
            let at = ours
                .iter()
                .zip(real)
                .position(|(a, b)| a != b)
                .unwrap_or(ours.len().min(real.len()));
            panic!(
                "{name} byte mismatch: ours.len={} real.len={} first diff at {}",
                ours.len(),
                real.len(),
                at
            );
        }
    }

    // ── MatIdMap unit tests ──────────────────────────────────────────────────

    #[test]
    fn mat_id_map_address_sorted_reachable_only() {
        // 5 objects: addresses out of order, object 2 is unreachable (idom==MAX)
        // dense ids:   0      1      2          3      4
        let addrs = [0x300u64, 0x100, 0x500, 0x200, 0x400];
        // idom: 0→vroot(5), 1→0, 2→UNREACHABLE, 3→0, 4→0, vroot→vroot
        let mut idom = vec![5u32, 0, u32::MAX, 0, 0, 5];
        idom[5] = 5; // virtual root self-loop

        let map = MatIdMap::build(5, &idom, |i| addrs[i]);

        // mat-id 0 = synthetic; reachable sorted by addr: 1(0x100) 3(0x200) 0(0x300) 4(0x400)
        assert_eq!(map.mat_count(), 5); // 4 reachable + synthetic id-0
        assert_eq!(map.sorted(), &[1u32, 3, 0, 4]);

        // old-id 1 → mat-id 1 (smallest addr among reachable)
        assert_eq!(map.translate(1), 1);
        assert_eq!(map.translate(3), 2);
        assert_eq!(map.translate(0), 3);
        assert_eq!(map.translate(4), 4);
        // unreachable object 2 → -1
        assert_eq!(map.translate(2), -1);
        // out-of-range → -1
        assert_eq!(map.translate(-1), -1);
        assert_eq!(map.translate(99), -1);
    }

    #[test]
    fn mat_id_map_all_reachable() {
        // 3 objects, all reachable, already address-sorted
        let addrs = [0x10u64, 0x20, 0x30];
        let idom = vec![3u32, 3, 3, 3]; // all dominated by vroot (index 3)
        let map = MatIdMap::build(3, &idom, |i| addrs[i]);
        assert_eq!(map.mat_count(), 4);
        assert_eq!(map.translate(0), 1);
        assert_eq!(map.translate(1), 2);
        assert_eq!(map.translate(2), 3);
    }

    #[test]
    fn mat_id_map_empty_graph() {
        // n=0: no objects at all; mat_count = 1 (synthetic root only)
        let idom: Vec<u32> = vec![];
        let map = MatIdMap::build(0, &idom, |_| 0);
        assert_eq!(map.mat_count(), 1);
        assert_eq!(map.sorted(), &[] as &[u32]);
        assert_eq!(map.translate(0), -1);
        assert_eq!(map.addr_at_mat(0), 0);
    }

    #[test]
    fn mat_id_map_all_unreachable() {
        // 3 objects, all unreachable (idom==MAX)
        let idom = vec![u32::MAX; 3];
        let map = MatIdMap::build(3, &idom, |i| i as u64);
        assert_eq!(map.mat_count(), 1); // only synthetic root
        assert_eq!(map.sorted(), &[] as &[u32]);
        assert_eq!(map.translate(0), -1);
        assert_eq!(map.translate(1), -1);
    }

    #[test]
    fn mat_id_map_addr_at_mat_bounds() {
        let addrs = [0xAAu64, 0xBB, 0xCC];
        let idom = vec![3u32, 3, 3, 3];
        let map = MatIdMap::build(3, &idom, |i| addrs[i]);
        // mat-id 0 = synthetic root (addr 0x0)
        assert_eq!(map.addr_at_mat(0), 0);
        // mat-ids 1..=3 get the sorted addresses
        assert_eq!(map.addr_at_mat(1), 0xAA);
        assert_eq!(map.addr_at_mat(2), 0xBB);
        assert_eq!(map.addr_at_mat(3), 0xCC);
        // out-of-range
        assert_eq!(map.addr_at_mat(4), 0);
        assert_eq!(map.addr_at_mat(-1), 0);
    }

    #[test]
    fn size_compress_at_exact_boundary() {
        // 0x4_0000_0000 is the last value to use the /8 encoding
        let boundary = 0x4_0000_0000i64;
        let expected = ((boundary / 8) as i32).wrapping_add(0x7000_0000);
        assert_eq!(size_compress(boundary), expected);
        // one over → cap
        assert_eq!(size_compress(boundary + 1), 0xf000_0000u32 as i32);
        // one under i32::MAX boundary → identity
        assert_eq!(size_compress(i32::MAX as i64), i32::MAX);
        // one over i32::MAX → /8 encoding
        let just_over = i32::MAX as i64 + 1;
        assert_eq!(size_compress(just_over), ((just_over / 8) as i32).wrapping_add(0x7000_0000));
    }

    // ── emit_i2sv2 ──────────────────────────────────────────────────────────

    #[test]
    fn emit_i2sv2_empty_produces_empty_file() {
        let tmp = std::env::temp_dir().join("mat_i2sv2_empty");
        let _ = std::fs::remove_dir_all(&tmp);
        let e = MatEmitter::new(&tmp, "dump_", None).unwrap();
        e.emit_i2sv2(std::iter::empty()).unwrap();
        let path = tmp.join("dump_.i2sv2.index");
        let bytes = std::fs::read(&path).unwrap();
        assert!(bytes.is_empty(), "empty iterator should produce empty file");
    }

    #[test]
    fn emit_i2sv2_encoding() {
        // Three entries: (class_mat_id, retained) → file = (i32_be, i64_neg_be) x 3
        let entries = vec![(1i32, 100i64), (7i32, 200i64), (-1i32, 0i64)];
        let tmp = std::env::temp_dir().join("mat_i2sv2_encoding");
        let _ = std::fs::remove_dir_all(&tmp);
        let e = MatEmitter::new(&tmp, "dump_", None).unwrap();
        e.emit_i2sv2(entries.iter().copied()).unwrap();
        let bytes = std::fs::read(tmp.join("dump_.i2sv2.index")).unwrap();
        assert_eq!(bytes.len(), 3 * 12, "each entry is 4 + 8 bytes");
        // First entry: class_mat_id=1, retained=100 → stored as -100
        assert_eq!(&bytes[0..4], &1i32.to_be_bytes());
        assert_eq!(&bytes[4..12], &(-100i64).to_be_bytes());
        // Second entry
        assert_eq!(&bytes[12..16], &7i32.to_be_bytes());
        assert_eq!(&bytes[16..24], &(-200i64).to_be_bytes());
        // Third (retained=0 → stored as 0, negated)
        assert_eq!(&bytes[24..28], &(-1i32).to_be_bytes());
        assert_eq!(&bytes[28..36], &0i64.to_be_bytes());
    }

    #[test]
    fn emit_i2sv2_roundtrip_size() {
        // File size must always be a multiple of 12.
        let entries: Vec<(i32, i64)> = (0..50).map(|i| (i, i as i64 * 1000)).collect();
        let tmp = std::env::temp_dir().join("mat_i2sv2_roundtrip");
        let _ = std::fs::remove_dir_all(&tmp);
        let e = MatEmitter::new(&tmp, "dump_", None).unwrap();
        e.emit_i2sv2(entries.iter().copied()).unwrap();
        let bytes = std::fs::read(tmp.join("dump_.i2sv2.index")).unwrap();
        assert_eq!(bytes.len() % 12, 0);
        assert_eq!(bytes.len() / 12, 50);
    }

    // ── emit_threads ────────────────────────────────────────────────────────

    fn make_thread_stack(serial: u32, thread_obj_idx: u32, frames: &[&str]) -> crate::pass2::ThreadStack {
        crate::pass2::ThreadStack {
            thread_serial: serial,
            thread_obj_idx,
            frames: frames.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn emit_threads_empty_produces_empty_file() {
        // No thread stacks at all → file should be empty (0 bytes, no content).
        let idom = vec![0u32, 0];
        let mm = MatIdMap::build(2, &idom, |i| (i as u64 + 1) * 0x10);
        let tmp = std::env::temp_dir().join("mat_threads_empty");
        let _ = std::fs::remove_dir_all(&tmp);
        let e = MatEmitter::new(&tmp, "dump_", None).unwrap();
        e.emit_threads(&[], &mm, &HashMap::new()).unwrap();
        let bytes = std::fs::read(tmp.join("dump_.threads")).unwrap();
        assert!(bytes.is_empty(), "no threads → empty file");
    }

    #[test]
    fn emit_threads_skips_stack_with_no_frames() {
        // A ThreadStack with empty frames vec should be silently skipped.
        let ts = make_thread_stack(1, 0, &[]); // no frames
        let idom = vec![0u32, 0];
        let mm = MatIdMap::build(2, &idom, |i| (i as u64 + 1) * 0x10);
        let tmp = std::env::temp_dir().join("mat_threads_skip_noframe");
        let _ = std::fs::remove_dir_all(&tmp);
        let e = MatEmitter::new(&tmp, "dump_", None).unwrap();
        e.emit_threads(&[ts], &mm, &HashMap::new()).unwrap();
        let bytes = std::fs::read(tmp.join("dump_.threads")).unwrap();
        assert!(bytes.is_empty(), "thread with no frames should be skipped");
    }

    #[test]
    fn emit_threads_single_thread_address_and_frames() {
        // Thread obj_idx=0, addr=0x10 (mat-id 1). Two frames.
        let addrs = [0x10u64, 0x20];
        let idom = vec![2u32, 2, 2]; // vroot at idx 2
        let mm = MatIdMap::build(2, &idom, |i| addrs[i]);
        // dense-id 0 → mat-id 1, addr=0x10
        assert_eq!(mm.translate(0), 1);

        let ts = make_thread_stack(1, 0, &["com.example.Foo.bar(Foo.java:42)", "com.example.Main.main(Main.java:10)"]);
        let tmp = std::env::temp_dir().join("mat_threads_single");
        let _ = std::fs::remove_dir_all(&tmp);
        let e = MatEmitter::new(&tmp, "dump_", None).unwrap();
        e.emit_threads(&[ts], &mm, &HashMap::new()).unwrap();
        let content = std::fs::read_to_string(tmp.join("dump_.threads")).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines[0], "Thread 0x10");
        assert_eq!(lines[1], "  at com.example.Foo.bar(Foo.java:42)");
        assert_eq!(lines[2], "  at com.example.Main.main(Main.java:10)");
        // trailing blank line
        assert!(content.ends_with('\n'));
    }

    #[test]
    fn emit_threads_unreachable_thread_uses_addr_zero() {
        // Thread obj_idx=99 (out of range / unreachable) → thread addr should be 0.
        let idom = vec![0u32, 0]; // only 2 objects
        let mm = MatIdMap::build(2, &idom, |i| (i as u64 + 1) * 0x100);
        let ts = make_thread_stack(5, 99, &["java.lang.Thread.run(Thread.java:1)"]);
        let tmp = std::env::temp_dir().join("mat_threads_unreachable");
        let _ = std::fs::remove_dir_all(&tmp);
        let e = MatEmitter::new(&tmp, "dump_", None).unwrap();
        e.emit_threads(&[ts], &mm, &HashMap::new()).unwrap();
        let content = std::fs::read_to_string(tmp.join("dump_.threads")).unwrap();
        assert!(content.starts_with("Thread 0x0\n"), "unreachable thread obj → addr 0, got: {content:?}");
    }

    #[test]
    fn emit_threads_with_locals() {
        // Thread with locals section: frame_num and local object addresses.
        let addrs = [0x100u64, 0x200, 0x300];
        let idom = vec![3u32, 3, 3, 3];
        let mm = MatIdMap::build(3, &idom, |i| addrs[i]);
        // old_id 0 → mat-id 1 (addr 0x100), old_id 2 → mat-id 3 (addr 0x300)
        let ts = make_thread_stack(1, 0, &["frame1(A.java:1)"]);
        let mut locals: HashMap<u32, Vec<(u32, u32)>> = HashMap::new();
        // serial=1, frame_num=0 (→ line 1), local old_idx=2
        locals.insert(1, vec![(0u32, 2u32), (u32::MAX, 1u32)]);
        let tmp = std::env::temp_dir().join("mat_threads_locals");
        let _ = std::fs::remove_dir_all(&tmp);
        let e = MatEmitter::new(&tmp, "dump_", None).unwrap();
        e.emit_threads(&[ts], &mm, &locals).unwrap();
        let content = std::fs::read_to_string(tmp.join("dump_.threads")).unwrap();
        assert!(content.contains("locals:"), "should have locals section");
        assert!(content.contains("objectId=0x300, line=1"), "frame_num=0 → line 1, local at 0x300");
        assert!(content.contains("objectId=0x200, line=0"), "frame_num=MAX → line 0");
    }

    #[test]
    fn emit_threads_multiple_threads_separated_by_blank_lines() {
        let addrs = [0x10u64, 0x20, 0x30];
        let idom = vec![3u32, 3, 3, 3];
        let mm = MatIdMap::build(3, &idom, |i| addrs[i]);
        let stacks = vec![
            make_thread_stack(1, 0, &["a.A.run(A.java:1)"]),
            make_thread_stack(2, 1, &["b.B.run(B.java:2)"]),
        ];
        let tmp = std::env::temp_dir().join("mat_threads_multi");
        let _ = std::fs::remove_dir_all(&tmp);
        let e = MatEmitter::new(&tmp, "dump_", None).unwrap();
        e.emit_threads(&stacks, &mm, &HashMap::new()).unwrap();
        let content = std::fs::read_to_string(tmp.join("dump_.threads")).unwrap();
        assert!(content.contains("Thread 0x10"), "first thread");
        assert!(content.contains("Thread 0x20"), "second thread");
        // Each thread block ends with a blank line → content has at least 2 blank lines
        let blank_lines = content.lines().filter(|l| l.is_empty()).count();
        assert!(blank_lines >= 2, "expected blank lines between threads, got {blank_lines}");
    }

    // ---- emit_outbound / emit_inbound / emit_dom_out roundtrip tests ----

    fn read_sorted_1n(path: &std::path::Path) -> Vec<Vec<i32>> {
        use crate::mat::int_index_1n;
        let bytes = std::fs::read(path).unwrap();
        let n = bytes.len();
        let divider = i64::from_be_bytes(bytes[n - 8..n].try_into().unwrap()) as usize;

        fn parse_footer(region: &[u8]) -> (usize, i32, i64, Vec<i64>) {
            let n = region.len();
            let size = i32::from_be_bytes(region[n - 4..n].try_into().unwrap());
            let page_size = i32::from_be_bytes(region[n - 8..n - 4].try_into().unwrap());
            let pages = (size as usize).div_ceil(page_size as usize);
            let entries = pages + 1;
            let footer_start = n - 8 - entries * 8;
            let mut starts = Vec::with_capacity(entries);
            for i in 0..entries {
                let off = footer_start + i * 8;
                starts.push(i64::from_be_bytes(region[off..off + 8].try_into().unwrap()));
            }
            (pages, page_size, size as i64, starts)
        }

        fn decode_region(file: &[u8], region: &[u8], pages: usize, psize: i32, size: i64, starts: &[i64]) -> Vec<i32> {
            use crate::mat::codec::decode_int;
            let mut out = Vec::with_capacity(size as usize);
            for i in 0..pages {
                let s = starts[i] as usize;
                let e = starts[i + 1] as usize;
                let n = ((psize as usize)).min(size as usize - i * psize as usize);
                out.extend_from_slice(&decode_int(&file[s..e], n));
            }
            out
        }

        fn sorted_get(hdr: &[i32], body: &[i32], idx: usize) -> Vec<i32> {
            let p0 = hdr[idx] as i64;
            if p0 == 0 { return vec![]; }
            let body_end = (body.len() + 1) as i64;
            let mut p1 = if idx + 1 < hdr.len() { hdr[idx + 1] as i64 } else { body_end };
            let mut j = idx + 2;
            while p1 < p0 && j < hdr.len() { p1 = hdr[j] as i64; j += 1; }
            if p1 < p0 { p1 = body_end; }
            let s = (p0 - 1) as usize;
            body[s..s + (p1 - p0) as usize].to_vec()
        }

        let body_region = &bytes[0..divider];
        let (bp, bps, bs, bst) = parse_footer(body_region);
        let body_vals = decode_region(&bytes, body_region, bp, bps, bs, &bst);

        let hdr_region = &bytes[divider..n - 8];
        let (hp, hps, hs, hst) = parse_footer(hdr_region);
        let hst_local: Vec<i64> = hst.iter().map(|&s| s - divider as i64).collect();
        let hdr_vals = decode_region(hdr_region, hdr_region, hp, hps, hs, &hst_local);

        (0..hdr_vals.len()).map(|i| sorted_get(&hdr_vals, &body_vals, i)).collect()
    }

    fn read_unsorted_1n(path: &std::path::Path) -> Vec<Vec<i32>> {
        let bytes = std::fs::read(path).unwrap();
        let n = bytes.len();
        let divider = i64::from_be_bytes(bytes[n - 8..n].try_into().unwrap()) as usize;

        fn parse_footer(region: &[u8]) -> (usize, i32, i64, Vec<i64>) {
            let n = region.len();
            let size = i32::from_be_bytes(region[n - 4..n].try_into().unwrap());
            let page_size = i32::from_be_bytes(region[n - 8..n - 4].try_into().unwrap());
            let pages = (size as usize).div_ceil(page_size as usize);
            let entries = pages + 1;
            let footer_start = n - 8 - entries * 8;
            let mut starts = Vec::with_capacity(entries);
            for i in 0..entries {
                let off = footer_start + i * 8;
                starts.push(i64::from_be_bytes(region[off..off + 8].try_into().unwrap()));
            }
            (pages, page_size, size as i64, starts)
        }

        fn decode_region(file: &[u8], pages: usize, psize: i32, size: i64, starts: &[i64]) -> Vec<i32> {
            use crate::mat::codec::decode_int;
            let mut out = Vec::with_capacity(size as usize);
            for i in 0..pages {
                let s = starts[i] as usize;
                let e = starts[i + 1] as usize;
                let n = (psize as usize).min(size as usize - i * psize as usize);
                out.extend_from_slice(&decode_int(&file[s..e], n));
            }
            out
        }

        let body_region = &bytes[0..divider];
        let (bp, bps, bs, bst) = parse_footer(body_region);
        let body_vals = decode_region(&bytes, bp, bps, bs, &bst);

        let hdr_region = &bytes[divider..n - 8];
        let (hp, hps, hs, hst) = parse_footer(hdr_region);
        let hst_local: Vec<i64> = hst.iter().map(|&s| s - divider as i64).collect();
        let hdr_vals = decode_region(hdr_region, hp, hps, hs, &hst_local);

        let mut out = Vec::with_capacity(hdr_vals.len());
        for &pos in &hdr_vals {
            let p = pos as usize;
            let len = body_vals[p] as usize;
            out.push(body_vals[p + 1..p + 1 + len].to_vec());
        }
        out
    }

    #[test]
    fn emit_outbound_roundtrip() {
        let entries: Vec<Vec<i32>> = vec![
            vec![5, 3, 1],  // object 0: class-ref + 2 refs
            vec![],         // object 1: no outbound refs (hole)
            vec![7, 2],     // object 2: class-ref + 1 ref
            vec![9],        // object 3: class-ref only
        ];
        let tmp = std::env::temp_dir().join("mat_emit_outbound_rt");
        let _ = std::fs::remove_dir_all(&tmp);
        let e = MatEmitter::new(&tmp, "dump_", None).unwrap();
        e.emit_outbound(&entries).unwrap();
        let path = tmp.join("dump_.outbound.index");
        let recon = read_sorted_1n(&path);
        assert_eq!(recon, entries, "outbound sorted roundtrip");
    }

    #[test]
    fn emit_inbound_roundtrip() {
        let entries: Vec<Vec<i32>> = vec![
            vec![],           // object 0: no inbound (hole)
            vec![0, 2],       // object 1: referenced by 0 and 2
            vec![0],          // object 2: referenced by 0
            vec![],           // object 3: hole
        ];
        let tmp = std::env::temp_dir().join("mat_emit_inbound_rt");
        let _ = std::fs::remove_dir_all(&tmp);
        let e = MatEmitter::new(&tmp, "dump_", None).unwrap();
        e.emit_inbound(&entries).unwrap();
        let path = tmp.join("dump_.inbound.index");
        let recon = read_sorted_1n(&path);
        assert_eq!(recon, entries, "inbound sorted roundtrip");
    }

    #[test]
    fn emit_dom_out_roundtrip() {
        // domOut is UNSORTED: entries[0] = superroot children, entries[k+1] = children of k.
        let entries: Vec<Vec<i32>> = vec![
            vec![1, 2],    // superroot children: objects 1 and 2
            vec![3],       // object 0 dominated by: 3
            vec![],        // object 1: no dominated children
            vec![],        // object 2: no dominated children
            vec![],        // object 3: no dominated children
        ];
        let tmp = std::env::temp_dir().join("mat_emit_domout_rt");
        let _ = std::fs::remove_dir_all(&tmp);
        let e = MatEmitter::new(&tmp, "dump_", None).unwrap();
        e.emit_dom_out(&entries).unwrap();
        let path = tmp.join("dump_.domOut.index");
        let recon = read_unsorted_1n(&path);
        assert_eq!(recon, entries, "domOut unsorted roundtrip");
    }

    #[test]
    fn emit_outbound_all_empty_roundtrip() {
        let entries: Vec<Vec<i32>> = vec![vec![], vec![], vec![]];
        let tmp = std::env::temp_dir().join("mat_emit_outbound_empty");
        let _ = std::fs::remove_dir_all(&tmp);
        let e = MatEmitter::new(&tmp, "dump_", None).unwrap();
        e.emit_outbound(&entries).unwrap();
        let recon = read_sorted_1n(&tmp.join("dump_.outbound.index"));
        assert_eq!(recon, entries);
    }

    #[test]
    fn emit_inbound_all_empty_roundtrip() {
        let entries: Vec<Vec<i32>> = vec![vec![], vec![], vec![]];
        let tmp = std::env::temp_dir().join("mat_emit_inbound_empty");
        let _ = std::fs::remove_dir_all(&tmp);
        let e = MatEmitter::new(&tmp, "dump_", None).unwrap();
        e.emit_inbound(&entries).unwrap();
        let recon = read_sorted_1n(&tmp.join("dump_.inbound.index"));
        assert_eq!(recon, entries);
    }

    #[test]
    fn emit_dom_out_all_empty_roundtrip() {
        let entries: Vec<Vec<i32>> = vec![vec![], vec![], vec![]];
        let tmp = std::env::temp_dir().join("mat_emit_domout_empty");
        let _ = std::fs::remove_dir_all(&tmp);
        let e = MatEmitter::new(&tmp, "dump_", None).unwrap();
        e.emit_dom_out(&entries).unwrap();
        let recon = read_unsorted_1n(&tmp.join("dump_.domOut.index"));
        assert_eq!(recon, entries);
    }
}

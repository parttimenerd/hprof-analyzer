//! `jsonify(x [, depth])` — expand a heap object into JSON, following
//! reference fields **recursively**.
//!
//! `SELECT toString(s) FROM java.lang.String s` decodes one String.
//! `SELECT jsonify(s) FROM com.acme.FareSearchRequest s` is the next step up:
//! the object's own fields, and then *the fields of the objects those fields
//! point at* — `dep` is not `{City @ 0x1573cf870}` but the City's own
//! `value: "HRB"`. One query, no second round-trip.
//!
//! # How it reads
//!
//! Through [`crate::objread`] + the cache's `offsets.bin`, exactly like
//! `heap inspect`: a `seek` plus a few dozen bytes per object. It never
//! re-scans the dump, which is what makes it safe to put in front of the
//! server — the failure mode this exists to avoid is `toString(s)`'s, where one
//! query re-scans 44 GB and serializes behind the server's single query slot.
//!
//! # Why it is bounded (and why the bounds are loud)
//!
//! An object graph is not a tree: it has cycles, and it can be enormous. Three
//! independent caps keep one `jsonify` from becoming the next `toString`:
//!
//! * **depth** — [`DEFAULT_DEPTH`] levels below the root (overridable per call
//!   with `jsonify(s, 3)`; [`MAX_DEPTH`] is the hard ceiling);
//! * **object budget** — [`DEFAULT_MAX_OBJECTS`] objects decoded per column;
//! * **cell budget** — [`DEFAULT_MAX_CELLS`] result rows expanded per column.
//!
//! Plus a cycle guard: an object already on the current expansion path renders
//! as a reference marked `"@cycle": true` rather than recursing.
//!
//! Nothing is dropped silently. A reference left unexpanded carries
//! `"@notExpanded"` saying which cap stopped it, and the root object carries
//! `"@truncated"` naming the cap and the counts. All three caps have environment
//! overrides ([`DEPTH_ENV`] etc.) for the server, where the query text may not
//! be under the caller's control.
//!
//! # Reading cost, stated plainly
//!
//! `offsets.bin` is an `i64`-delta stream (see `CacheDir::write_hprof_offsets`),
//! so reaching object *i* means decoding from object 0. For a dump small enough
//! to hold the whole table in memory the table is read once and every lookup is
//! free. Above [`FULL_TABLE_MAX_OBJECTS`] the table is **streamed**, one pass per
//! expansion level — so the resident cost stays constant instead of the ~4.8 GB
//! a full `Vec<u64>` costs on a 600M-object dump. Expansion is breadth-first
//! across all rows of a column, so the pass count is `depth` per column, not
//! `depth × rows`.
//!
//! # Availability
//!
//! The reader needs (a) a seekable, uncompressed dump and (b) an `offsets.bin`.
//! When either is missing the cell says so — it does not come back empty. The
//! same holds on targets without the `native` feature (WASM), which has no
//! filesystem to seek in.

use crate::query::ast::{Query, SelectItem};
use crate::query::model::{QueryResult, QueryValue};

/// The field reader, re-declared into this module tree.
///
/// `src/objread.rs` is a module of the *library* crate (`lib.rs` has
/// `pub mod objread;`). The `hprof-analyzer` **binary** builds its own module
/// tree — `main.rs` declares `mod cache;`, `mod pass1;`, `mod types;` and so on,
/// but not `mod objread;` — and reaches the reader through
/// `hprof_analyzer::objread` instead. A path that resolves inside the library
/// does not resolve in the binary and vice versa, and this file is compiled into
/// both. Declaring the same source file here gives both trees a
/// `crate::query::jsonify::objread` to call — the same duplication `cache.rs`,
/// `pass1.rs` and `types.rs` already live with. It is the identical file, so
/// there is nothing to keep in sync.
#[cfg(feature = "native")]
#[path = "../objread.rs"]
#[allow(dead_code)] // this copy uses a subset of the reader's surface
#[allow(clippy::duplicate_mod)] // deliberate: see the note above
mod objread;

/// Reference levels expanded below the root by default.
///
/// The decoder walks one level *past* this so that a `String` sitting exactly at
/// the boundary still has its text attached. That is what makes the intended
/// shape work: `jsonify(request)` expands `dep` into the City's own fields, and
/// the City's `value` reads `"HRB"` rather than `byte[3]`.
pub const DEFAULT_DEPTH: u32 = 2;

/// Hard ceiling on `jsonify(x, depth)`. Beyond a handful of levels the output
/// stops being readable long before it stops being expensive.
pub const MAX_DEPTH: u32 = 8;

/// Objects decoded per jsonify column before expansion stops.
pub const DEFAULT_MAX_OBJECTS: usize = 100;

/// Result rows expanded per jsonify column. Without this a query matching a
/// million objects would expand a million trees — each bounded, but the total
/// is not.
pub const DEFAULT_MAX_CELLS: usize = 64;

/// Object count above which `offsets.bin` is streamed rather than held whole.
/// 8M × 8 B = 64 MB resident, which is fine; the 44 GB dump's ~600M objects
/// would be ~4.8 GB, which is not.
pub const FULL_TABLE_MAX_OBJECTS: usize = 8_000_000;

/// Environment override for [`DEFAULT_DEPTH`] (clamped to `1..=MAX_DEPTH`).
pub const DEPTH_ENV: &str = "HPROF_JSONIFY_DEPTH";
/// Environment override for [`DEFAULT_MAX_OBJECTS`].
pub const MAX_OBJECTS_ENV: &str = "HPROF_JSONIFY_MAX_OBJECTS";
/// Environment override for [`DEFAULT_MAX_CELLS`].
pub const MAX_CELLS_ENV: &str = "HPROF_JSONIFY_MAX_CELLS";

/// The cache has no `offsets.bin` — same wording family as `heap inspect`'s note.
pub const NO_OFFSETS_NOTE: &str = "this cache has no object-offset table (offsets.bin), so jsonify \
     cannot read objects. Re-analyze the dump to capture it.";
/// `offsets.bin` indexes the *decompressed* stream, so a gzip/zip dump is not
/// seekable — refuse rather than silently decompress everything before the offset.
pub const COMPRESSED_NOTE: &str = "the dump is compressed (gzip/zip), so record offsets are not \
     directly seekable; uncompress it and re-analyze to use jsonify.";
/// The scan path keeps no pass-1 tables after the scan, so it has nothing to
/// decode fields with. Say so rather than returning an empty object.
pub const NO_SESSION_NOTE: &str = "jsonify needs the cached pass-1 tables and offsets.bin; the \
     dump-scan path keeps neither. Analyze this dump first (`heap analyze` / `heap load`), then \
     query it through the server or the REPL.";
/// Targets built without the `native` feature (WASM) have no filesystem to seek in.
#[allow(dead_code)] // only reachable from the non-native build, so unused under `native`
pub const NO_NATIVE_NOTE: &str = "jsonify needs a seekable dump on disk; this build has no \
     filesystem support (no `native` feature).";

/// Depth used when the query does not pass one.
pub fn default_depth() -> u32 {
    std::env::var(DEPTH_ENV)
        .ok()
        .and_then(|v| v.trim().parse::<u32>().ok())
        .map(|d| d.clamp(1, MAX_DEPTH))
        .unwrap_or(DEFAULT_DEPTH)
}

/// Object budget used for one jsonify column.
pub fn max_objects() -> usize {
    std::env::var(MAX_OBJECTS_ENV)
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(DEFAULT_MAX_OBJECTS)
}

/// Row budget used for one jsonify column.
pub fn max_cells() -> usize {
    std::env::var(MAX_CELLS_ENV)
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(DEFAULT_MAX_CELLS)
}

/// The scan-time value of a `jsonify(...)` column.
///
/// The executor has no dump handle, so it cannot expand — it emits this
/// placeholder and [`expand_results`] replaces it after the scan. It is a JSON
/// *string* rather than an `ObjRef` on purpose: any path that does not run the
/// post-pass then shows an explicit `"@jsonify": "not expanded"` marker instead
/// of an object reference that a reader could mistake for the answer. The
/// placeholder carries the dense index the expansion needs.
pub fn pending_cell(idx: usize, class: &str, addr: Option<u64>) -> String {
    serde_json::json!({
        "@jsonify": "not expanded",
        "@index": idx,
        "@class": class,
        "@address": addr.map(|a| format!("0x{a:x}")),
    })
    .to_string()
}

/// The dense index carried by a [`pending_cell`], or `None` for anything else —
/// including an already-expanded cell, which makes the post-pass idempotent.
fn pending_index(cell: &QueryValue) -> Option<usize> {
    let QueryValue::Str(s) = cell else {
        return None;
    };
    let v: serde_json::Value = serde_json::from_str(s).ok()?;
    if v.get("@jsonify")?.as_str()? != "not expanded" {
        return None;
    }
    v.get("@index")?.as_u64().map(|i| i as usize)
}

/// Everything `jsonify` needs to read objects, in one place.
///
/// Implemented for `ReplCache` (the resident/server path — see the impl below).
/// A scan-path caller has no session to hand over; see [`expand_results`].
pub trait JsonifySession {
    /// The dump the object offsets index into. Must be uncompressed for the
    /// seeks to mean anything; `objread::is_seekable` is checked by the caller.
    fn dump_path(&self) -> &std::path::Path;
    fn class_map(&self) -> &std::collections::HashMap<u64, crate::pass1::ClassInfo>;
    fn strings(&self) -> &std::collections::HashMap<u64, String>;
    fn id_size(&self) -> usize;
    /// Number of dense object indices — the length `offsets.bin` should have.
    fn object_count(&self) -> usize;
    /// 0 = instance, 1 = object array, 2 = primitive array, 3 = class object.
    fn kind_of(&self, idx: usize) -> u8;
    fn index_of_addr(&self, addr: u64) -> Option<usize>;
    /// Dotted class name of a dense object index, for display.
    fn class_name_of_index(&self, idx: usize) -> Option<String>;
}

impl JsonifySession for crate::query::run::ReplCache {
    fn dump_path(&self) -> &std::path::Path {
        std::path::Path::new(self.source.file_path())
    }
    fn class_map(&self) -> &std::collections::HashMap<u64, crate::pass1::ClassInfo> {
        &self.p1.class_map
    }
    fn strings(&self) -> &std::collections::HashMap<u64, String> {
        &self.p1.strings
    }
    fn id_size(&self) -> usize {
        self.id_size
    }
    fn object_count(&self) -> usize {
        self.n
    }
    fn kind_of(&self, idx: usize) -> u8 {
        self.p1.kind.get(idx).copied().unwrap_or(0)
    }
    fn index_of_addr(&self, addr: u64) -> Option<usize> {
        self.p1.id_map.index_of(addr)
    }
    fn class_name_of_index(&self, idx: usize) -> Option<String> {
        let ci = *self.class_idx.get(idx)?;
        self.class_names.get(ci as usize).cloned()
    }
}

/// The `(column, depth)` pairs of a query's jsonify projections.
fn jsonify_columns(q: &Query) -> Vec<(usize, u32)> {
    q.select
        .iter()
        .enumerate()
        .filter_map(|(i, it)| match it {
            SelectItem::Jsonify { depth, .. } => Some((i, depth.unwrap_or_else(default_depth))),
            _ => None,
        })
        .collect()
}

/// Expand every `jsonify(...)` column of `results`.
///
/// `results` and `flat` are parallel — one result per flat slot, *before*
/// UNION-collapse — which is what lets a column be matched to its `SelectItem`
/// exactly rather than by parsing the rendered column name back. Callers must
/// therefore run this before [`crate::query::run::collapse_union_results`].
///
/// `sess` is `None` when the caller has no object-reading context (the
/// dump-scan path keeps no pass-1 tables after the scan). That is a **loud**
/// degradation: each jsonify cell is replaced by a one-line explanation and the
/// result carries the same text as its note.
pub fn expand_results(
    results: &mut [QueryResult],
    flat: &[(Query, crate::query::plan::QueryPlan)],
    sess: Option<&dyn JsonifySession>,
) {
    if results.len() != flat.len() {
        // Defensive: the caller passed non-parallel slices, so column identity
        // cannot be established. Leave the values alone rather than guess.
        return;
    }
    for (slot, (q, _)) in flat.iter().enumerate() {
        let cols = jsonify_columns(q);
        if cols.is_empty() {
            continue;
        }
        #[cfg(feature = "native")]
        imp::expand_slot(&mut results[slot], &cols, sess);
        #[cfg(not(feature = "native"))]
        {
            let _ = &sess;
            imp::mark_unavailable(&mut results[slot], &cols, NO_NATIVE_NOTE);
        }
    }
}

#[cfg(not(feature = "native"))]
mod imp {
    use super::*;

    pub(super) fn mark_unavailable(res: &mut QueryResult, cols: &[(usize, u32)], why: &str) {
        for &(col, _) in cols {
            for row in res.rows.iter_mut() {
                if let Some(cell) = row.get_mut(col) {
                    *cell = QueryValue::Str(format!("[jsonify unavailable: {why}]"));
                }
            }
        }
        push_note(res, why);
    }
}

#[cfg(feature = "native")]
mod imp {
    use super::objread::{self, FieldValue};
    use super::*;
    use serde_json::{Map, Value, json};
    use std::collections::{HashMap, HashSet};
    use std::fs::File;
    use std::io::{self, BufReader, Read, Seek, SeekFrom};
    use std::path::{Path, PathBuf};

    /// One decoded object, before its children are spliced in.
    enum Shallow {
        /// A `java.lang.String`. Its backing array is read separately (see
        /// `attach_string_texts`) so the text is available even when the
        /// backing array would sit one level past the depth cap.
        Str {
            addr: u64,
            text: Option<(String, u64, bool)>,
        },
        Instance {
            addr: u64,
            class: String,
            fields: Vec<(String, FieldNode)>,
        },
        Array {
            addr: u64,
            kind: &'static str,
            elem_class: Option<String>,
            elem_type: Option<&'static str>,
            count: u64,
            truncated: bool,
            elems: Vec<FieldNode>,
        },
        ClassObject {
            class: String,
        },
        /// No HPROF record (or none recorded) — reported, never rendered empty.
        Opaque {
            class: String,
            why: String,
        },
    }

    enum FieldNode {
        Value(Value),
        Ref {
            idx: Option<usize>,
            addr: u64,
            class: Option<String>,
        },
    }

    /// A String whose text still has to be decoded.
    struct StringJob {
        node: usize,
        value_idx: Option<usize>,
        coder: i64,
        offset: Option<i64>,
        count: Option<i64>,
    }

    // ── Object offsets ───────────────────────────────────────────────────────

    /// `offsets.bin` lookups: whole table when it fits, streamed otherwise.
    struct OffsetTable {
        full: Option<Vec<u64>>,
        stream: Option<PathBuf>,
        memo: HashMap<usize, u64>,
    }

    impl OffsetTable {
        fn open(dump: &Path, n: usize) -> io::Result<Option<Self>> {
            let dir = crate::cache::CacheDir::for_dump(dump)?;
            if !dir.has_hprof_offsets() {
                return Ok(None);
            }
            if n <= FULL_TABLE_MAX_OBJECTS {
                return Ok(dir.read_hprof_offsets()?.map(|v| Self {
                    full: Some(v),
                    stream: None,
                    memo: HashMap::new(),
                }));
            }
            Ok(Some(Self {
                full: None,
                stream: Some(dir.path.join("offsets.bin")),
                memo: HashMap::new(),
            }))
        }

        fn get(&self, idx: usize) -> Option<u64> {
            match &self.full {
                Some(v) => v.get(idx).copied(),
                None => self.memo.get(&idx).copied(),
            }
        }

        /// One streaming pass for every not-yet-memoized index in `idxs`.
        /// A no-op when the whole table is resident.
        fn prefetch(&mut self, idxs: &[usize]) -> io::Result<()> {
            if self.full.is_some() {
                return Ok(());
            }
            let Some(bin) = self.stream.clone() else {
                return Ok(());
            };
            let mut want: Vec<usize> = idxs
                .iter()
                .copied()
                .filter(|i| !self.memo.contains_key(i))
                .collect();
            if want.is_empty() {
                return Ok(());
            }
            want.sort_unstable();
            want.dedup();
            for (i, v) in stream_offsets(&bin, &want)? {
                self.memo.insert(i, v);
            }
            Ok(())
        }
    }

    /// Locate an entry in an HPCA indexed file and seek to its zstd frame.
    ///
    /// Mirrors the private reader in `src/cache.rs`. Duplicated deliberately:
    /// `cache.rs` only exposes a whole-`Vec<u64>` read, which is exactly the
    /// ~4.8 GB this module exists to avoid on a 600M-object dump. The right
    /// long-term home for this is a `CacheDir::read_hprof_offset_at`; until
    /// then this file is the only place that knows the layout, and
    /// `stream_offsets_round_trips_cache_writer` pins it against the writer.
    pub(super) fn open_hpca_entry(path: &Path, name: &str) -> io::Result<Option<(File, u64)>> {
        let mut f = File::open(path)?;
        let mut head = [0u8; 7];
        if f.read_exact(&mut head).is_err() || &head[0..4] != b"HPCA" {
            return Ok(None);
        }
        if u16::from_le_bytes([head[4], head[5]]) != 1 {
            return Ok(None);
        }
        let n = head[6] as usize;
        let mut index = vec![0u8; n * 32];
        f.read_exact(&mut index)?;
        let want = name.as_bytes();
        for e in index.chunks_exact(32) {
            let (nm, rest) = e.split_at(16);
            if nm.starts_with(want) && nm[want.len()..].iter().all(|&b| b == 0) {
                let off = u64::from_le_bytes(rest[0..8].try_into().expect("8 bytes"));
                let len = u64::from_le_bytes(rest[8..16].try_into().expect("8 bytes"));
                f.seek(SeekFrom::Start(off))?;
                return Ok(Some((f, len)));
            }
        }
        Ok(None)
    }

    /// Decode the `offsets` delta stream up to the largest wanted index,
    /// keeping only the requested values. `want` must be sorted and deduped.
    pub(super) fn stream_offsets(bin: &Path, want: &[usize]) -> io::Result<HashMap<usize, u64>> {
        let mut out = HashMap::with_capacity(want.len());
        let Some((f, len)) = open_hpca_entry(bin, "offsets")? else {
            return Ok(out);
        };
        let mut r = BufReader::new(zstd::stream::read::Decoder::new(f.take(len))?);
        let max = *want.iter().max().expect("caller filters empties");
        let mut cursor = 0usize;
        let mut acc: i64 = 0;
        let mut buf = [0u8; 8];
        for i in 0..=max {
            r.read_exact(&mut buf)?;
            acc = acc.wrapping_add(i64::from_le_bytes(buf));
            while cursor < want.len() && want[cursor] < i {
                cursor += 1;
            }
            if cursor < want.len() && want[cursor] == i {
                out.insert(i, acc as u64);
                cursor += 1;
            }
        }
        Ok(out)
    }

    // ── Decode ───────────────────────────────────────────────────────────────

    struct Decoder<'a> {
        sess: &'a dyn JsonifySession,
        path: PathBuf,
        offsets: OffsetTable,
        max_objects: usize,
        nodes: HashMap<usize, Shallow>,
        /// A String's backing `value` array: read with a text-sized cap.
        string_backing: HashSet<usize>,
        string_jobs: Vec<StringJob>,
        budget_hit: bool,
    }

    impl<'a> Decoder<'a> {
        fn new(
            sess: &'a dyn JsonifySession,
            path: &Path,
            offsets: OffsetTable,
            max_objects: usize,
        ) -> Self {
            Self {
                sess,
                path: path.to_path_buf(),
                offsets,
                max_objects,
                nodes: HashMap::new(),
                string_backing: HashSet::new(),
                string_jobs: Vec::new(),
                budget_hit: false,
            }
        }

        /// Breadth-first decode of `roots` and everything reachable within
        /// `depth` levels, stopping at `max_objects`.
        ///
        /// Breadth-first, not depth-first, is what keeps the streaming cost at
        /// one pass per level for the whole column: every object at a given
        /// level is prefetched together.
        fn decode_all(&mut self, roots: &[usize], depth: u32) -> io::Result<()> {
            let mut level: Vec<usize> = Vec::new();
            let mut seen: HashSet<usize> = HashSet::new();
            for &r in roots {
                if seen.insert(r) {
                    level.push(r);
                }
            }
            for _ in 0..=depth {
                if level.is_empty() || self.nodes.len() >= self.max_objects {
                    self.budget_hit = self.nodes.len() >= self.max_objects && !level.is_empty();
                    break;
                }
                self.offsets.prefetch(&level)?;
                let mut next = Vec::new();
                for idx in level {
                    if self.nodes.len() >= self.max_objects {
                        self.budget_hit = true;
                        break;
                    }
                    if self.nodes.contains_key(&idx) {
                        continue;
                    }
                    let (node, children) = self.decode_one(idx)?;
                    for c in children {
                        if seen.insert(c) {
                            next.push(c);
                        }
                    }
                    self.nodes.insert(idx, node);
                }
                level = next;
            }
            self.attach_string_texts()?;
            Ok(())
        }

        fn decode_one(&mut self, idx: usize) -> io::Result<(Shallow, Vec<usize>)> {
            let class = self
                .sess
                .class_name_of_index(idx)
                .unwrap_or_else(|| format!("<index {idx}>"));
            let off = self.offsets.get(idx).filter(|&o| o != 0);
            let Some(off) = off else {
                return Ok((
                    Shallow::Opaque {
                        class,
                        why: "no HPROF record offset for this object".into(),
                    },
                    Vec::new(),
                ));
            };
            match self.sess.kind_of(idx) {
                3 => Ok((Shallow::ClassObject { class }, Vec::new())),
                0 => self.decode_instance(idx, off, class),
                _ => self.decode_array(idx, off, class),
            }
        }

        fn decode_instance(
            &mut self,
            idx: usize,
            off: u64,
            class: String,
        ) -> io::Result<(Shallow, Vec<usize>)> {
            let read = objread::read_instance_at(
                &self.path,
                off,
                self.sess.class_map(),
                self.sess.strings(),
                self.sess.id_size(),
            );
            let Some((addr, _class_addr, fields)) = (match read {
                Ok(v) => v,
                Err(e) => {
                    return Ok((
                        Shallow::Opaque {
                            class,
                            why: format!("could not decode this object: {e}"),
                        },
                        Vec::new(),
                    ));
                }
            }) else {
                return Ok((
                    Shallow::Opaque {
                        class,
                        why: "no INSTANCE_DUMP record at the recorded offset".into(),
                    },
                    Vec::new(),
                ));
            };

            if class.replace('/', ".") == "java.lang.String" {
                let (mut value_idx, mut coder, mut joff, mut jcount) = (None, 0i64, None, None);
                for f in &fields {
                    match (f.name.as_str(), &f.value) {
                        ("value", FieldValue::Ref(a)) => {
                            value_idx = self.sess.index_of_addr(*a);
                            if let Some(i) = value_idx {
                                self.string_backing.insert(i);
                            }
                        }
                        ("coder", FieldValue::Prim(v)) => coder = v.as_i64().unwrap_or(0),
                        ("offset", FieldValue::Prim(v)) => joff = v.as_i64(),
                        ("count", FieldValue::Prim(v)) => jcount = v.as_i64(),
                        _ => {}
                    }
                }
                self.string_jobs.push(StringJob {
                    node: idx,
                    value_idx,
                    coder,
                    offset: joff,
                    count: jcount,
                });
                return Ok((Shallow::Str { addr, text: None }, Vec::new()));
            }

            // A subclass field may shadow an inherited one of the same simple
            // name; qualify those with their declaring class so none is lost —
            // same rule as `heap inspect`.
            let mut counts: HashMap<&str, usize> = HashMap::new();
            for f in &fields {
                *counts.entry(f.name.as_str()).or_insert(0) += 1;
            }
            let mut out = Vec::with_capacity(fields.len());
            let mut children = Vec::new();
            for f in &fields {
                let key = if counts[f.name.as_str()] > 1 {
                    format!("{}.{}", f.owner.replace('/', "."), f.name)
                } else {
                    f.name.clone()
                };
                let node = match &f.value {
                    FieldValue::Prim(v) => FieldNode::Value(v.clone()),
                    FieldValue::Null => FieldNode::Value(Value::Null),
                    FieldValue::Ref(a) => {
                        let tgt = self.sess.index_of_addr(*a);
                        if let Some(i) = tgt {
                            children.push(i);
                        }
                        let cls = tgt.and_then(|i| self.sess.class_name_of_index(i));
                        FieldNode::Ref {
                            idx: tgt,
                            addr: *a,
                            class: cls,
                        }
                    }
                };
                out.push((key, node));
            }
            Ok((
                Shallow::Instance {
                    addr,
                    class: class.replace('/', "."),
                    fields: out,
                },
                children,
            ))
        }

        fn decode_array(
            &mut self,
            idx: usize,
            off: u64,
            class: String,
        ) -> io::Result<(Shallow, Vec<usize>)> {
            let cap = if self.string_backing.contains(&idx) {
                objread::MAX_STRING_CHARS
            } else {
                objread::MAX_ARRAY_ELEMENTS
            };
            let read = objread::read_array_at(&self.path, off, self.sess.id_size(), 0, cap);
            let Some(arr) = (match read {
                Ok(v) => v,
                Err(e) => {
                    return Ok((
                        Shallow::Opaque {
                            class,
                            why: format!("could not decode this array: {e}"),
                        },
                        Vec::new(),
                    ));
                }
            }) else {
                return Ok((
                    Shallow::Opaque {
                        class,
                        why: "no array record at the recorded offset".into(),
                    },
                    Vec::new(),
                ));
            };
            let elem_class = (arr.elem_ty.is_none())
                .then(|| {
                    self.sess
                        .class_map()
                        .get(&arr.elem_class_addr)
                        .and_then(|ci| self.sess.strings().get(&ci.name_id).cloned())
                        .map(|n| n.replace('/', "."))
                })
                .flatten();
            let mut elems = Vec::with_capacity(arr.values.len());
            let mut children = Vec::new();
            for v in &arr.values {
                elems.push(match v {
                    FieldValue::Prim(v) => FieldNode::Value(v.clone()),
                    FieldValue::Null => FieldNode::Value(Value::Null),
                    FieldValue::Ref(a) => {
                        let tgt = self.sess.index_of_addr(*a);
                        if let Some(i) = tgt {
                            children.push(i);
                        }
                        let cls = tgt.and_then(|i| self.sess.class_name_of_index(i));
                        FieldNode::Ref {
                            idx: tgt,
                            addr: *a,
                            class: cls,
                        }
                    }
                });
            }
            Ok((
                Shallow::Array {
                    addr: arr.addr,
                    kind: if arr.elem_ty.is_some() {
                        "primitive array"
                    } else {
                        "object array"
                    },
                    elem_class,
                    elem_type: arr.elem_ty.map(type_name),
                    count: arr.count,
                    truncated: arr.truncated(),
                    elems,
                },
                children,
            ))
        }

        /// Decode the text of every String node.
        ///
        /// Separate from the level walk on purpose: a String's backing array
        /// sits one level below the String, so at the depth cap it would never
        /// be reached — and the whole point of `jsonify` is that
        /// `dep.value` reads `"HRB"`, not `byte[3]`.
        fn attach_string_texts(&mut self) -> io::Result<()> {
            if self.string_jobs.is_empty() {
                return Ok(());
            }
            let wants: Vec<usize> = self
                .string_jobs
                .iter()
                .filter_map(|j| j.value_idx)
                .collect();
            self.offsets.prefetch(&wants)?;
            let id_size = self.sess.id_size();
            for job in std::mem::take(&mut self.string_jobs) {
                let Some(vi) = job.value_idx else { continue };
                let Some(voff) = self.offsets.get(vi).filter(|&o| o != 0) else {
                    continue;
                };
                // Pre-9 a String may share a `char[]` and own a window of it.
                let skip = job.offset.unwrap_or(0).max(0) as u64;
                let Ok(Some(arr)) = objread::read_array_at(
                    &self.path,
                    voff,
                    id_size,
                    skip,
                    objread::MAX_STRING_CHARS,
                ) else {
                    continue;
                };
                let Some(ty) = arr.elem_ty else { continue };
                let text = match ty {
                    crate::types::HprofType::Byte => {
                        objread::text_from_bytes(&arr.values, job.coder)
                    }
                    crate::types::HprofType::Char => {
                        let n = job
                            .count
                            .map(|c| c.max(0) as usize)
                            .unwrap_or(arr.values.len())
                            .min(arr.values.len());
                        objread::text_from_chars(&arr.values[..n])
                    }
                    _ => continue,
                };
                let full = full_char_len(ty, job.coder, arr.count, job.count);
                let truncated = full > text.chars().count() as u64;
                let text: String = text.chars().take(objread::MAX_STRING_CHARS).collect();
                if let Some(Shallow::Str { text: slot, .. }) = self.nodes.get_mut(&job.node) {
                    *slot = Some((text, full, truncated));
                }
            }
            Ok(())
        }

        fn render(&self, idx: usize, depth: u32) -> Value {
            let mut on_path = Vec::new();
            let mut st = RenderState {
                depth_cap: depth,
                object_limit: self.max_objects,
                budget: self.budget_hit,
                depth_cut: false,
                cycle: false,
                expanded: self.nodes.len(),
            };
            let mut v = render_index(&self.nodes, idx, depth, &mut on_path, &mut st);
            if let Value::Object(o) = &mut v {
                let mut cuts: Vec<String> = Vec::new();
                if st.budget {
                    cuts.push(format!(
                        "object budget reached ({} objects decoded, limit {}); the remaining \
                         references are marked @notExpanded",
                        st.expanded, self.max_objects
                    ));
                }
                if st.depth_cut {
                    cuts.push(format!("depth limit reached ({} levels)", st.depth_cap));
                }
                if st.cycle {
                    cuts.push("a reference cycle was cut".to_string());
                }
                if !cuts.is_empty() {
                    o.insert("@truncated".into(), json!(cuts.join("; ")));
                }
            }
            v
        }
    }

    struct RenderState {
        depth_cap: u32,
        object_limit: usize,
        budget: bool,
        depth_cut: bool,
        cycle: bool,
        expanded: usize,
    }

    fn compact_ref(
        idx: Option<usize>,
        addr: u64,
        class: Option<&str>,
        why: Option<String>,
    ) -> Value {
        let mut o = Map::new();
        o.insert("@ref".into(), json!(format!("0x{addr:x}")));
        if let Some(i) = idx {
            o.insert("@index".into(), json!(i));
        }
        if let Some(c) = class {
            o.insert("@class".into(), json!(c));
        }
        if let Some(w) = why {
            o.insert("@notExpanded".into(), json!(w));
        }
        Value::Object(o)
    }

    fn render_index(
        nodes: &HashMap<usize, Shallow>,
        idx: usize,
        depth_left: u32,
        on_path: &mut Vec<usize>,
        st: &mut RenderState,
    ) -> Value {
        let Some(node) = nodes.get(&idx) else {
            st.budget = true;
            return compact_ref(
                Some(idx),
                0,
                None,
                Some(format!(
                    "object budget reached (limit {}), not decoded",
                    st.object_limit
                )),
            );
        };
        match node {
            // A String is a leaf: its text is the whole answer, and rendering it
            // costs no further reads, so the depth cap does not apply.
            Shallow::Str { addr, text } => match text {
                Some((t, full, cut)) => {
                    let mut o = Map::new();
                    o.insert("@address".into(), json!(format!("0x{addr:x}")));
                    o.insert("@class".into(), json!("java.lang.String"));
                    o.insert("@text".into(), json!(t));
                    if *cut {
                        o.insert("@text_length".into(), json!(full));
                    }
                    Value::Object(o)
                }
                None => {
                    let mut o = Map::new();
                    o.insert("@address".into(), json!(format!("0x{addr:x}")));
                    o.insert("@class".into(), json!("java.lang.String"));
                    o.insert("@text".into(), Value::Null);
                    o.insert(
                        "@text_note".into(),
                        json!("text not decodable (no offset for the backing array)"),
                    );
                    Value::Object(o)
                }
            },
            Shallow::ClassObject { class } => json!({
                "@class": class,
                "@kind": "class object",
                "@note": "class objects have no HPROF record to decode",
            }),
            Shallow::Opaque { class, why } => json!({ "@class": class, "@note": why }),
            Shallow::Instance {
                addr,
                class,
                fields,
            } => {
                if on_path.contains(&idx) {
                    st.cycle = true;
                    return compact_ref(Some(idx), *addr, Some(class), Some("cycle".into()));
                }
                if depth_left == 0 {
                    st.depth_cut = true;
                    return compact_ref(
                        Some(idx),
                        *addr,
                        Some(class),
                        Some(format!("depth limit ({})", st.depth_cap)),
                    );
                }
                on_path.push(idx);
                let mut o = Map::new();
                o.insert("@address".into(), json!(format!("0x{addr:x}")));
                o.insert("@class".into(), json!(class));
                for (k, f) in fields {
                    let v = render_field(nodes, f, depth_left - 1, on_path, st);
                    o.insert(k.clone(), v);
                }
                on_path.pop();
                Value::Object(o)
            }
            Shallow::Array {
                addr,
                kind,
                elem_class,
                elem_type,
                count,
                truncated,
                elems,
            } => {
                if on_path.contains(&idx) {
                    st.cycle = true;
                    return compact_ref(Some(idx), *addr, None, Some("cycle".into()));
                }
                if depth_left == 0 {
                    st.depth_cut = true;
                    return compact_ref(
                        Some(idx),
                        *addr,
                        elem_class.as_deref(),
                        Some(format!("depth limit ({})", st.depth_cap)),
                    );
                }
                on_path.push(idx);
                let mut o = Map::new();
                o.insert("@address".into(), json!(format!("0x{addr:x}")));
                o.insert("@kind".into(), json!(kind));
                if let Some(t) = elem_type {
                    o.insert("@element_type".into(), json!(t));
                }
                if let Some(c) = elem_class {
                    o.insert("@element_class".into(), json!(c));
                }
                o.insert("@element_count".into(), json!(count));
                let rendered: Vec<Value> = elems
                    .iter()
                    .map(|f| render_field(nodes, f, depth_left - 1, on_path, st))
                    .collect();
                o.insert("elements".into(), Value::Array(rendered));
                if *truncated {
                    o.insert(
                        "@elements_truncated".into(),
                        json!(format!(
                            "showing the first {} of {count} elements",
                            elems.len()
                        )),
                    );
                }
                on_path.pop();
                Value::Object(o)
            }
        }
    }

    fn render_field(
        nodes: &HashMap<usize, Shallow>,
        f: &FieldNode,
        depth_left: u32,
        on_path: &mut Vec<usize>,
        st: &mut RenderState,
    ) -> Value {
        match f {
            FieldNode::Value(v) => v.clone(),
            FieldNode::Ref { idx, addr, class } => match idx {
                Some(i) => match nodes.get(i) {
                    Some(_) => render_index(nodes, *i, depth_left, on_path, st),
                    // The address resolved to a dense index but the budget ran
                    // out before it was decoded: say so, do not render a bare ref.
                    None => {
                        st.budget = true;
                        compact_ref(
                            Some(*i),
                            *addr,
                            class.as_deref(),
                            Some(format!(
                                "object budget reached (limit {}), not decoded",
                                st.object_limit
                            )),
                        )
                    }
                },
                // The address is not a heap object in this dump (a JNI/foreign
                // pointer, or a reference the id-map does not cover).
                None => compact_ref(
                    None,
                    *addr,
                    class.as_deref(),
                    Some("address is not a heap object".into()),
                ),
            },
        }
    }

    fn type_name(ty: crate::types::HprofType) -> &'static str {
        use crate::types::HprofType as T;
        match ty {
            T::Object => "object",
            T::Boolean => "boolean",
            T::Char => "char",
            T::Float => "float",
            T::Double => "double",
            T::Byte => "byte",
            T::Short => "short",
            T::Int => "int",
            T::Long => "long",
        }
    }

    fn full_char_len(
        ty: crate::types::HprofType,
        coder: i64,
        arr_count: u64,
        jcount: Option<i64>,
    ) -> u64 {
        use crate::types::HprofType as T;
        match ty {
            T::Byte if coder == 1 => arr_count / 2,
            T::Byte => arr_count,
            T::Char => jcount.map(|c| c.max(0) as u64).unwrap_or(arr_count),
            _ => arr_count,
        }
    }

    // ── Post-pass ────────────────────────────────────────────────────────────

    /// Replace the scan-time [`pending_cell`] placeholder of each jsonify cell
    /// with the expanded JSON string.
    ///
    /// The placeholder is a JSON string on purpose: it carries the dense index
    /// the expansion needs, and if this pass ever fails to run the cell reads as
    /// an explicit `"@jsonify": "not expanded"` rather than as a plausible-
    /// looking object reference.
    pub(super) fn expand_slot(
        res: &mut QueryResult,
        cols: &[(usize, u32)],
        sess: Option<&dyn JsonifySession>,
    ) {
        expand_slot_with(res, cols, sess, max_objects(), max_cells());
    }

    /// [`expand_slot`] with the budgets injected, so the caps can be tested
    /// without touching process-global environment variables.
    pub(super) fn expand_slot_with(
        res: &mut QueryResult,
        cols: &[(usize, u32)],
        sess: Option<&dyn JsonifySession>,
        object_budget: usize,
        cell_budget: usize,
    ) {
        let Some(sess) = sess else {
            mark_unavailable(res, cols, NO_SESSION_NOTE);
            return;
        };
        match objread::is_seekable(sess.dump_path()) {
            Ok(true) => {}
            Ok(false) => {
                mark_unavailable(res, cols, COMPRESSED_NOTE);
                return;
            }
            Err(e) => {
                mark_unavailable(
                    res,
                    cols,
                    &format!(
                        "could not open the dump at {}: {e}",
                        sess.dump_path().display()
                    ),
                );
                return;
            }
        }
        let path = sess.dump_path().to_path_buf();
        for &(col, depth) in cols {
            let table = match OffsetTable::open(&path, sess.object_count()) {
                Ok(Some(t)) => t,
                Ok(None) => {
                    mark_unavailable(res, &[(col, depth)], NO_OFFSETS_NOTE);
                    continue;
                }
                Err(e) => {
                    mark_unavailable(
                        res,
                        &[(col, depth)],
                        &format!("could not read the object-offset table: {e}"),
                    );
                    continue;
                }
            };
            // (row, dense index) for every cell we will expand. The index comes
            // out of the scan-time placeholder — see `pending_cell`.
            let mut roots: Vec<(usize, usize)> = Vec::new();
            let mut cells_total = 0usize;
            for (r, row) in res.rows.iter().enumerate() {
                let Some(cell) = row.get(col) else {
                    continue;
                };
                let Some(idx) = pending_index(cell) else {
                    continue;
                };
                cells_total += 1;
                roots.push((r, idx));
            }
            let cap = cell_budget;
            let overflow = roots.len().saturating_sub(cap);
            roots.truncate(cap);
            let mut dec = Decoder::new(sess, &path, table, object_budget);
            let want: Vec<usize> = roots.iter().map(|r| r.1).collect();
            if let Err(e) = dec.decode_all(&want, depth) {
                mark_unavailable(
                    res,
                    &[(col, depth)],
                    &format!("could not read object offsets: {e}"),
                );
                continue;
            }
            for (r, idx) in &roots {
                if let Some(row) = res.rows.get_mut(*r) {
                    if col < row.len() {
                        row[col] = QueryValue::Str(dec.render(*idx, depth).to_string());
                    }
                }
            }
            if overflow > 0 {
                res.truncated = true;
                push_note(
                    res,
                    &format!(
                        "jsonify expanded {cap} of {cells_total} rows (cell limit); the rest are \
                         left as unexpanded references. Raise {} to expand more.",
                        super::MAX_CELLS_ENV
                    ),
                );
            }
        }
    }

    pub(super) fn mark_unavailable(res: &mut QueryResult, cols: &[(usize, u32)], why: &str) {
        for &(col, _) in cols {
            for row in res.rows.iter_mut() {
                if let Some(cell) = row.get_mut(col) {
                    *cell = QueryValue::Str(format!("[jsonify unavailable: {why}]"));
                }
            }
        }
        push_note(res, why);
    }
}

/// Append `note` to a result's advisory note, keeping any existing text.
fn push_note(res: &mut QueryResult, note: &str) {
    res.note = Some(match res.note.take() {
        Some(prev) if !prev.is_empty() => format!("{prev}; {note}"),
        _ => note.to_string(),
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::ast::{ClassSpec, FromSource};
    use crate::query::model::QueryColumn;

    /// A one-column `SELECT jsonify(s) FROM <class> s` result whose only cell is
    /// the scan-time placeholder for `idx`. The depth is not part of the
    /// placeholder (the executor never sees it) — it is taken from the `Query`,
    /// which is why the callers pass it to `jsonify_query` as well.
    fn jsonify_cell(idx: u64, class: &str, _depth: Option<u32>) -> QueryResult {
        QueryResult {
            name: "q1".into(),
            oql: "SELECT jsonify(s) FROM C s".into(),
            columns: vec![QueryColumn {
                name: "jsonify(s)".into(),
            }],
            rows: vec![vec![QueryValue::Str(pending_cell(
                idx as usize,
                class,
                None,
            ))]],
            row_count: 1,
            truncated: false,
            error: None,
            note: None,
            viz: None,
            elapsed_ms: None,
        }
    }

    fn jsonify_query(class: &str, depth: Option<u32>) -> Query {
        Query {
            distinct: false,
            select: vec![SelectItem::Jsonify {
                alias: "s".into(),
                depth,
            }],
            select_aliases: vec![None],
            retained_set: false,
            from: FromSource::Class(ClassSpec {
                instanceof: false,
                class_name: class.into(),
                is_regex: false,
            }),
            alias: Some("s".into()),
            where_: None,
            order_by: None,
            limit: None,
            offset: None,
            union_branches: vec![],
            union_limit: None,
            group_by: vec![],
            having: None,
            intersect_branches: vec![],
            except_branches: vec![],
        }
    }

    /// The `jsonify` cell of row 0, parsed back from its JSON text.
    fn cell_json(res: &QueryResult) -> serde_json::Value {
        match &res.rows[0][0] {
            QueryValue::Str(s) => serde_json::from_str(s).expect("jsonify cell must be JSON"),
            other => panic!("jsonify cell must be a JSON string, got {other:?}"),
        }
    }

    // ── Syntax ───────────────────────────────────────────────────────────────

    /// Both spellings parse, and the depth lands in the AST.
    #[test]
    fn parses_with_and_without_depth() {
        let q = crate::query::parse::parse("SELECT jsonify(s) FROM C s").unwrap();
        assert_eq!(
            q.select,
            vec![SelectItem::Jsonify {
                alias: "s".into(),
                depth: None
            }]
        );
        let q = crate::query::parse::parse("SELECT jsonify(s, 3) FROM C s").unwrap();
        assert_eq!(
            q.select,
            vec![SelectItem::Jsonify {
                alias: "s".into(),
                depth: Some(3)
            }]
        );
        // Case-insensitive, like every other built-in.
        assert!(
            crate::query::parse::parse("SELECT JSONIFY(s) FROM C s").is_ok(),
            "jsonify is case-insensitive"
        );
    }

    /// A depth outside `1..=MAX_DEPTH` is a parse error, not a silent clamp —
    /// `jsonify(s, 0)` would otherwise read as "expand nothing".
    #[test]
    fn rejects_out_of_range_depth() {
        let err = crate::query::parse::parse("SELECT jsonify(s, 0) FROM C s").unwrap_err();
        assert!(
            err.to_string().contains("between 1 and"),
            "0 must be rejected with an actionable message: {err}"
        );
        let too_deep = format!("SELECT jsonify(s, {}) FROM C s", MAX_DEPTH + 1);
        assert!(crate::query::parse::parse(&too_deep).is_err());
    }

    /// The column header shows the depth that was written, so two jsonify
    /// columns at different depths are distinguishable in the output.
    #[test]
    fn column_name_carries_the_depth() {
        use crate::query::execute::column_name;
        assert_eq!(
            column_name(&SelectItem::Jsonify {
                alias: "s".into(),
                depth: None
            }),
            "jsonify(s)"
        );
        assert_eq!(
            column_name(&SelectItem::Jsonify {
                alias: "s".into(),
                depth: Some(3)
            }),
            "jsonify(s, 3)"
        );
    }

    /// jsonify must set **no** `needs` flag: that is what keeps a jsonify query
    /// resident-only, so the server answers it from the warm cache instead of
    /// re-scanning the dump. If this ever flips, jsonify silently becomes the
    /// expensive query it exists to avoid.
    #[test]
    fn jsonify_stays_resident_only() {
        let q = crate::query::parse::parse("SELECT jsonify(s) FROM C s").unwrap();
        let plan =
            crate::query::plan::plan_query(&q, crate::query::DEFAULT_PATH_DEPTH_CAP).unwrap();
        assert!(
            plan.is_resident_only(),
            "jsonify must not arm any cross-phase need"
        );
    }

    /// Aggregate arguments are rejected rather than folded to Null.
    #[test]
    fn jsonify_is_rejected_as_an_aggregate_argument() {
        let q = crate::query::parse::parse("SELECT COUNT(jsonify(s)) FROM C s").unwrap();
        let err =
            crate::query::plan::plan_query(&q, crate::query::DEFAULT_PATH_DEPTH_CAP).unwrap_err();
        assert!(
            err.to_string().contains("aggregate argument"),
            "expected an actionable rejection, got: {err}"
        );
    }

    // ── Degradation ──────────────────────────────────────────────────────────

    /// With no session the cell explains itself. The failure this guards
    /// against is a jsonify column that comes back as a bare `obj_ref` (or,
    /// worse, as an empty object) with nothing saying the expansion never ran.
    #[test]
    fn no_session_degrades_loudly() {
        let mut results = [jsonify_cell(1, "com.acme.C", None)];
        let q = jsonify_query("com.acme.C", None);
        let plan = crate::query::plan::QueryPlan::default();
        expand_results(&mut results, &[(q, plan)], None);
        match &results[0].rows[0][0] {
            QueryValue::Str(s) => assert!(
                s.starts_with("[jsonify unavailable:"),
                "cell must say the expansion did not run, got: {s}"
            ),
            other => panic!("expected the explanation string, got {other:?}"),
        }
        assert!(
            results[0].note.as_deref().unwrap_or("").contains("pass-1"),
            "the note must say why: {:?}",
            results[0].note
        );
    }

    /// `expand_results` must not guess when the slices are not parallel.
    #[test]
    fn mismatched_slices_leave_results_untouched() {
        let mut results = [jsonify_cell(1, "com.acme.C", None)];
        expand_results(&mut results, &[], None);
        assert!(
            pending_index(&results[0].rows[0][0]).is_some(),
            "a non-parallel call must be a no-op, not a partial expansion"
        );
    }

    /// The streamed reader must agree with the writer in `cache.rs` — this is
    /// the only thing pinning the format this module re-implements.
    #[cfg(feature = "native")]
    #[test]
    fn stream_offsets_round_trips_the_cache_writer() {
        let dir = std::env::temp_dir().join(format!(
            "hprof_jsonify_offsets_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let cache = crate::cache::CacheDir { path: dir.clone() };
        // Deliberately not monotonic, and with a repeat: the encoder writes
        // wrapping i64 deltas, so both must survive.
        let offs: Vec<u64> = vec![0, 100, 100, 250_000, 3, 4_000_000_000, 5];
        cache.write_hprof_offsets(&offs).unwrap();
        let bin = dir.join("offsets.bin");

        let want = vec![0usize, 2, 3, 5, 6];
        let got = imp::stream_offsets(&bin, &want).unwrap();
        for i in want {
            assert_eq!(
                got[&i], offs[i],
                "streamed offset {i} must match the writer"
            );
        }
        // And the whole-table path agrees with it.
        let full = cache.read_hprof_offsets().unwrap().unwrap();
        assert_eq!(full, offs);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── End to end, on the real fixture ──────────────────────────────────────

    /// The fixture is 23 MB and ships a warm cache (including `offsets.bin`), so
    /// this reads it exactly the way the server does — no pipeline run.
    #[cfg(feature = "native")]
    fn fixture_cache() -> Option<crate::query::run::ReplCache> {
        let dump = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/dump_4_philosophers.hprof");
        match std::fs::metadata(&dump) {
            Ok(m) if m.len() >= 1024 => {}
            _ => return None,
        }
        let dir = crate::cache::CacheDir::for_dump(&dump).ok()?;
        if !dir.has_hprof_offsets() {
            return None;
        }
        let src = crate::source::HprofSource::from(dump.to_string_lossy().as_ref());
        dir.read_query_cache(src).ok().flatten()
    }

    /// The first dense index whose class is `name` (dotted form, as the cache's
    /// class-name table stores it). Lets a test name a class instead of a
    /// hard-coded index.
    #[cfg(feature = "native")]
    fn first_index_of(cache: &crate::query::run::ReplCache, name: &str) -> Option<usize> {
        let ci = cache.class_names.iter().position(|n| n == name)? as u32;
        cache.class_idx.iter().position(|c| *c == ci)
    }

    /// The headline behaviour: a reference field expands into the *target's*
    /// fields, and a String one hop down shows its text rather than `byte[]`.
    ///
    /// Object 100 of this fixture is a `java.util.HashMap$Node`: `key` and
    /// `value` are Strings, `next` is null. At the default depth 2 the Strings
    /// are decoded (level 1) and their text is attached — that is exactly the
    /// `dep` → `City { value: "HRB" }` shape, one level less deep.
    #[cfg(feature = "native")]
    #[test]
    fn expands_a_reference_field_into_the_targets_fields() {
        let Some(cache) = fixture_cache() else {
            return;
        };
        let mut results = [jsonify_cell(100, "java.util.HashMap$Node", None)];
        let q = jsonify_query("java.util.HashMap$Node", None);
        let plan = crate::query::plan::QueryPlan::default();
        expand_results(
            &mut results,
            &[(q, plan)],
            Some(&cache as &dyn JsonifySession),
        );
        let v = cell_json(&results[0]);

        assert_eq!(v["@class"], "java.util.HashMap$Node");
        assert!(v["@address"].is_string(), "the root carries its address");
        // Primitive and null fields keep their plain JSON shapes.
        assert_eq!(v["hash"], serde_json::json!(119777));
        assert_eq!(v["next"], serde_json::Value::Null);
        // A reference field is expanded, not left as an address...
        assert_eq!(v["key"]["@class"], "java.lang.String");
        assert!(
            v["key"].get("@ref").is_none(),
            "an expanded reference must not still read as a bare @ref: {}",
            v["key"]
        );
        // ...and a String one hop down shows its text, which is the whole point.
        assert!(
            v["key"]["@text"].is_string(),
            "a String reference must carry its decoded text: {}",
            v["key"]
        );
        assert!(
            v.get("@truncated").is_none(),
            "a 3-field object must not report truncation: {v}"
        );
        eprintln!("jsonify(HashMap$Node#100) = {v}");
    }

    /// Depth 1 stops one level short: a reference to a *non-leaf* object is
    /// rendered as a compact `@ref` marked with the reason, never silently
    /// dropped. A `String` is exempt (see the leaf rule) — its text is the whole
    /// answer and costs no further reads, so `jsonify(x, 1)` still reads its
    /// strings.
    ///
    /// The root here is a `java.util.HashMap`: its `table` is a `Node[]`, so at
    /// depth 1 the array is *decoded* (it has to be, to reach its element type)
    /// but not *expanded*.
    #[cfg(feature = "native")]
    #[test]
    fn depth_limit_is_marked_inline() {
        let Some(cache) = fixture_cache() else {
            return;
        };
        let Some(idx) = first_index_of(&cache, "java.util.HashMap") else {
            return;
        };
        let render = |depth: u32| -> serde_json::Value {
            let mut results = [jsonify_cell(idx as u64, "java.util.HashMap", Some(depth))];
            let q = jsonify_query("java.util.HashMap", Some(depth));
            let plan = crate::query::plan::QueryPlan::default();
            expand_results(
                &mut results,
                &[(q, plan)],
                Some(&cache as &dyn JsonifySession),
            );
            cell_json(&results[0])
        };

        let shallow = render(1);
        assert!(
            shallow["table"]["@notExpanded"]
                .as_str()
                .unwrap_or("")
                .contains("depth limit"),
            "an array past the depth cap must say so: {}",
            shallow["table"]
        );
        assert!(
            shallow["@truncated"]
                .as_str()
                .unwrap_or("")
                .contains("depth limit"),
            "the root must say the depth cap was hit: {shallow}"
        );
        assert_eq!(shallow["size"], serde_json::json!(9), "scalars still read");

        // One level deeper and the same array expands into its elements...
        let deeper = render(2);
        let elems = deeper["table"]["elements"]
            .as_array()
            .unwrap_or_else(|| panic!("table must expand at depth 2: {deeper}"));
        assert!(!elems.is_empty(), "the fixture's HashMap must hold entries");
        assert!(
            elems[0]["@notExpanded"]
                .as_str()
                .unwrap_or("")
                .contains("depth limit"),
            "an element at the new boundary must say so: {}",
            elems[0]
        );

        // ...and at depth 3 each element expands into its own fields. (In this
        // fixture the keys are `Locale`s, so they land on the new boundary and
        // are marked as such — which is the point: every cut is named.)
        let deepest = render(3);
        let elems = deepest["table"]["elements"]
            .as_array()
            .expect("table must still expand at depth 3");
        assert_eq!(elems[0]["@class"], "java.util.HashMap$Node");
        assert!(
            elems[0]["hash"].is_number(),
            "an element's primitive field must be read: {}",
            elems[0]
        );
        assert!(
            elems[0]["key"]["@notExpanded"]
                .as_str()
                .unwrap_or("")
                .contains("depth limit"),
            "a reference at the new boundary must say so: {}",
            elems[0]
        );
    }

    /// A String field is readable at *any* depth: it is a leaf, so expanding it
    /// is not "going deeper". `jsonify(x, 1)` on a `HashMap$Node` therefore shows
    /// both String fields in full, with no truncation report.
    #[cfg(feature = "native")]
    #[test]
    fn strings_are_leaves_at_any_depth() {
        let Some(cache) = fixture_cache() else {
            return;
        };
        let mut results = [jsonify_cell(100, "java.util.HashMap$Node", Some(1))];
        let q = jsonify_query("java.util.HashMap$Node", Some(1));
        let plan = crate::query::plan::QueryPlan::default();
        expand_results(
            &mut results,
            &[(q, plan)],
            Some(&cache as &dyn JsonifySession),
        );
        let v = cell_json(&results[0]);
        assert_eq!(v["key"]["@text"], "ymt");
        assert!(v["value"]["@text"].is_string(), "value must read as text");
        assert!(
            v.get("@truncated").is_none(),
            "nothing was cut: every field is a scalar, null or String: {v}"
        );
    }

    /// The object budget is reported, not silently applied. A budget of 1 lets
    /// only the root be decoded, so every reference past it is unexpandable and
    /// the root has to say so.
    #[cfg(feature = "native")]
    #[test]
    fn object_budget_is_reported() {
        let Some(cache) = fixture_cache() else {
            return;
        };
        let mut results = [jsonify_cell(100, "java.util.HashMap$Node", None)];
        // Budgets are injected rather than read from the environment: env vars
        // are process-global and the test harness runs these in parallel.
        imp::expand_slot_with(
            &mut results[0],
            &[(0, 2)],
            Some(&cache as &dyn JsonifySession),
            1,
            DEFAULT_MAX_CELLS,
        );
        let v = cell_json(&results[0]);
        assert!(
            v["@truncated"]
                .as_str()
                .unwrap_or("")
                .contains("object budget"),
            "the root must name the budget and the count: {v}"
        );
        assert!(
            v["key"]["@notExpanded"]
                .as_str()
                .unwrap_or("")
                .contains("object budget"),
            "the cut reference must say which cap stopped it: {}",
            v["key"]
        );
    }

    /// The whole path, not just the expander: parse → plan → the resident scan
    /// (which projects the placeholder) → the post-pass that turns it into JSON.
    /// This is the shape the server serves.
    #[cfg(feature = "native")]
    #[test]
    fn resident_path_answers_a_jsonify_query() {
        let Some(cache) = fixture_cache() else {
            return;
        };
        let q =
            crate::query::parse::parse_or_report("SELECT jsonify(s) FROM java.util.HashMap$Node s")
                .expect("parse");
        let plan = crate::query::plan::plan_query(&q, 0).expect("plan");
        let plan = crate::query::optimize::optimize(
            plan,
            &q,
            &crate::query::optimize::SchemaStats::default(),
        );
        let results = crate::query::run::run_resident_only(&cache, &[(q, plan)], false)
            .expect("resident run");
        assert_eq!(results.len(), 1);
        let res = &results[0];
        assert!(res.error.is_none(), "query errored: {:?}", res.error);
        assert_eq!(res.columns[0].name, "jsonify(s)");
        assert!(!res.rows.is_empty(), "the fixture has HashMap nodes");

        // The row budget applies to the whole column, so a query matching more
        // rows than `DEFAULT_MAX_CELLS` expands the first N and says so.
        let expanded = res.rows.len().min(DEFAULT_MAX_CELLS);
        let mut saw_a_string_field = false;
        for row in &res.rows[..expanded] {
            let QueryValue::Str(json) = &row[0] else {
                panic!(
                    "an expanded jsonify cell must be a JSON string, got {:?}",
                    row[0]
                );
            };
            let v: serde_json::Value = serde_json::from_str(json).expect("cell must be JSON");
            assert_eq!(
                v["@class"], "java.util.HashMap$Node",
                "the row's own object must be the root: {v}"
            );
            if v["key"]["@text"].is_string() || v["value"]["@text"].is_string() {
                saw_a_string_field = true;
            }
        }
        assert!(
            saw_a_string_field,
            "at least one node holds a String whose text must be decoded"
        );
        if res.rows.len() > expanded {
            assert!(res.truncated, "hitting the row budget is truncation");
            let note = res.note.as_deref().unwrap_or("");
            assert!(
                note.contains(&format!("{expanded} of {} rows", res.rows.len())),
                "the note must give the counts: {note}"
            );
            assert!(
                pending_index(&res.rows[expanded][0]).is_some(),
                "a row past the budget keeps its placeholder, never an empty cell"
            );
        }
    }

    /// The row budget bounds a query that matches many objects, and reports how
    /// many it did expand rather than quietly returning a partial column.
    #[cfg(feature = "native")]
    #[test]
    fn cell_budget_is_reported() {
        let Some(cache) = fixture_cache() else {
            return;
        };
        let mut res = jsonify_cell(100, "java.util.HashMap$Node", None);
        res.rows.push(res.rows[0].clone());
        res.row_count = 2;
        let mut results = [res];
        imp::expand_slot_with(
            &mut results[0],
            &[(0, 2)],
            Some(&cache as &dyn JsonifySession),
            DEFAULT_MAX_OBJECTS,
            1,
        );
        assert!(results[0].truncated, "hitting the row budget is truncation");
        let note = results[0].note.as_deref().unwrap_or("");
        assert!(
            note.contains("1 of 2 rows"),
            "the note must give the counts: {note}"
        );
        // The rows past the cap keep the scan-time placeholder, which is a
        // visible "not expanded" rather than an empty string.
        assert!(
            pending_index(&results[0].rows[1][0]).is_some(),
            "an unexpanded row must keep its placeholder: {:?}",
            results[0].rows[1][0]
        );
    }
}

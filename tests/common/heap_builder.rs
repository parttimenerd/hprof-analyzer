//! Minimal synthetic-HPROF scenario builder, used by `tests/scenarios.rs`.
//!
//! This is intentionally NOT a full general-purpose HPROF-emitting DSL —
//! per the plan's explicit guidance ("build only the primitives needed for
//! the first scenarios, generalize after, not before"), it supports just
//! enough shapes to cover: a static-field GC-root anchor, a self-referential
//! object tree/chain, a `java.util.ArrayList` with a pre-sized empty backing
//! array, and a `Thread` GC root transiting to a deeper real culprit. New
//! scenarios should extend this file's primitives only as actually needed,
//! not speculatively.
//!
//! The raw record-level encoders (`hprof_header`, `make_record`,
//! `make_class_dump`, etc.) are deliberately re-derived here rather than
//! `#[path]`-included from `tests/fuzzing.rs`: fuzzing.rs's copies are
//! `fn`-private to that binary and already duplicated (with a comment
//! explaining why) from `tests/integration.rs`'s — this is the third,
//! intentionally-independent copy following that established convention
//! ("cannot import across test binaries" without restructuring into a
//! shared lib, which is out of scope here).
//!
//! Wire-format reference: HPROF binary dump spec, record tags in
//! `hprof_analyzer::types::{tags, heap}`.

#![allow(dead_code)]

use std::collections::HashMap;

// ---------------------------------------------------------------------------
// Low-level record encoders (8-byte object IDs only — simplifies all offset
// math; the real analyzer supports 4-byte IDs too, exercised separately by
// tests/fuzzing.rs).
// ---------------------------------------------------------------------------

const ID_SIZE: usize = 8;

fn write_id(buf: &mut Vec<u8>, id: u64) {
    buf.extend_from_slice(&id.to_be_bytes());
}

fn hprof_header() -> Vec<u8> {
    let mut h = b"JAVA PROFILE 1.0.2\0".to_vec();
    h.extend_from_slice(&(ID_SIZE as u32).to_be_bytes());
    h.extend_from_slice(&0u64.to_be_bytes()); // timestamp
    h
}

fn make_record(tag: u8, body: &[u8]) -> Vec<u8> {
    let mut r = vec![tag];
    r.extend_from_slice(&0u32.to_be_bytes()); // ts_delta
    r.extend_from_slice(&(body.len() as u32).to_be_bytes());
    r.extend_from_slice(body);
    r
}

fn make_string(id: u64, text: &str) -> Vec<u8> {
    let mut body = Vec::new();
    write_id(&mut body, id);
    body.extend_from_slice(text.as_bytes());
    make_record(0x01, &body)
}

fn make_load_class(serial: u32, class_id: u64, name_id: u64) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&serial.to_be_bytes());
    write_id(&mut body, class_id);
    body.extend_from_slice(&0u32.to_be_bytes()); // stack_serial
    write_id(&mut body, name_id);
    make_record(0x02, &body)
}

/// One declared instance field: `(name_id, hprof_type_code)`. Type codes:
/// 2=Object, 4=Boolean, 5=Char, 6=Float, 7=Double, 8=Byte, 9=Short, 10=Int, 11=Long.
pub const T_OBJECT: u8 = 2;
pub const T_INT: u8 = 10;

fn make_class_dump(class_id: u64, super_id: u64, fields: &[(u64, u8)]) -> Vec<u8> {
    let mut body = Vec::new();
    write_id(&mut body, class_id);
    body.extend_from_slice(&1u32.to_be_bytes()); // serial
    write_id(&mut body, super_id);
    write_id(&mut body, 0); // loader
    write_id(&mut body, 0); // signers
    write_id(&mut body, 0); // domain
    write_id(&mut body, 0); // res1
    write_id(&mut body, 0); // res2
    body.extend_from_slice(&0u32.to_be_bytes()); // instance_size (unenforced)
    body.extend_from_slice(&0u16.to_be_bytes()); // cp_count
    body.extend_from_slice(&0u16.to_be_bytes()); // static_count
    body.extend_from_slice(&(fields.len() as u16).to_be_bytes());
    for &(name_id, type_code) in fields {
        write_id(&mut body, name_id);
        body.push(type_code);
    }
    let mut rec = vec![0x20u8];
    rec.extend_from_slice(&body);
    rec
}

/// CLASS_DUMP variant carrying one `Object`-typed static field pointing at
/// `static_val` — this is how scenarios anchor a graph to a GC root without
/// an explicit `ROOT_*` record: a class with no sticky-class roots present
/// anywhere in the dump is itself synthetically rooted (MAT-parity
/// `addSystemClassRootsIfMissing`, see `pass2::sizing::should_add_system_class_root`),
/// and this static field is then a real forward edge from that rooted class
/// object to whatever it points at.
fn make_class_dump_with_static(
    class_id: u64,
    super_id: u64,
    static_name_id: u64,
    static_val: u64,
    fields: &[(u64, u8)],
) -> Vec<u8> {
    let mut body = Vec::new();
    write_id(&mut body, class_id);
    body.extend_from_slice(&1u32.to_be_bytes());
    write_id(&mut body, super_id);
    write_id(&mut body, 0);
    write_id(&mut body, 0);
    write_id(&mut body, 0);
    write_id(&mut body, 0);
    write_id(&mut body, 0);
    body.extend_from_slice(&0u32.to_be_bytes());
    body.extend_from_slice(&0u16.to_be_bytes()); // cp_count
    body.extend_from_slice(&1u16.to_be_bytes()); // static_count = 1
    write_id(&mut body, static_name_id);
    body.push(T_OBJECT);
    write_id(&mut body, static_val);
    body.extend_from_slice(&(fields.len() as u16).to_be_bytes());
    for &(name_id, type_code) in fields {
        write_id(&mut body, name_id);
        body.push(type_code);
    }
    let mut rec = vec![0x20u8];
    rec.extend_from_slice(&body);
    rec
}

fn make_instance_dump(obj_id: u64, class_id: u64, data: &[u8]) -> Vec<u8> {
    let mut body = Vec::new();
    write_id(&mut body, obj_id);
    body.extend_from_slice(&0u32.to_be_bytes()); // serial
    write_id(&mut body, class_id);
    body.extend_from_slice(&(data.len() as u32).to_be_bytes());
    body.extend_from_slice(data);
    let mut rec = vec![0x21u8];
    rec.extend_from_slice(&body);
    rec
}

/// OBJ_ARRAY_DUMP: `obj_id + serial(4) + count(4) + elem_class_id + [count * id]`.
fn make_obj_array(obj_id: u64, elem_class_id: u64, elem_ids: &[u64]) -> Vec<u8> {
    let mut body = Vec::new();
    write_id(&mut body, obj_id);
    body.extend_from_slice(&0u32.to_be_bytes());
    body.extend_from_slice(&(elem_ids.len() as u32).to_be_bytes());
    write_id(&mut body, elem_class_id);
    for &id in elem_ids {
        write_id(&mut body, id);
    }
    let mut rec = vec![0x22u8];
    rec.extend_from_slice(&body);
    rec
}

/// ROOT_THREAD_OBJ: `obj_id + thread_serial(4) + stack_trace_serial(4)`.
fn make_root_thread_obj(obj_id: u64, thread_serial: u32) -> Vec<u8> {
    let mut rec = vec![0x08u8];
    write_id(&mut rec, obj_id);
    rec.extend_from_slice(&thread_serial.to_be_bytes());
    rec.extend_from_slice(&0u32.to_be_bytes()); // stack_trace_serial
    rec
}

fn make_heap_dump(sub_records: Vec<u8>) -> Vec<u8> {
    let mut out = make_record(0x0C, &sub_records);
    out.extend_from_slice(&make_record(0x2C, &[]));
    out
}

// ---------------------------------------------------------------------------
// Scenario builder
// ---------------------------------------------------------------------------

/// Incrementally assembles one synthetic `.hprof` byte buffer. IDs are
/// allocated monotonically starting at `0x1000`; call sites never need to
/// pick IDs themselves except when explicitly wiring one object to another
/// (e.g. a static root or a tree-child pointer), in which case the ID
/// returned by the creating call is reused directly.
pub struct HeapBuilder {
    next_id: u64,
    strings: Vec<(u64, String)>,
    string_ids: HashMap<String, u64>,
    load_classes: Vec<u8>,
    class_dumps: Vec<u8>,
    heap_subrecords: Vec<u8>,
    next_class_serial: u32,
    /// `java/lang/Object`'s class id, lazily created on first use and reused
    /// as every other declared class's `super_id`. Required because
    /// `calculate_size_recursive` (pass2::sizing) treats `super_id == 0` as
    /// "this class IS the Object root, header-only, ignore its own fields"
    /// — a class declared with `super_id = 0` silently gets ZERO shallow
    /// size for any fields it declares, which (among other things) makes
    /// self-referential-chain scenarios retain far too little to clear
    /// `SELF_REF_MIN_RETAINED`. Every class built by this module other than
    /// the Object root itself must chain up to this id.
    object_class_id: Option<u64>,
}

impl HeapBuilder {
    pub fn new() -> Self {
        Self {
            next_id: 0x1000,
            strings: Vec::new(),
            string_ids: HashMap::new(),
            load_classes: Vec::new(),
            class_dumps: Vec::new(),
            heap_subrecords: Vec::new(),
            next_class_serial: 1,
            object_class_id: None,
        }
    }

    fn alloc(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// Intern a UTF-8 string, returning its string-record id (reused on
    /// repeat calls with the same text).
    fn intern(&mut self, s: &str) -> u64 {
        if let Some(&id) = self.string_ids.get(s) {
            return id;
        }
        let id = self.alloc();
        self.strings.push((id, s.to_string()));
        self.string_ids.insert(s.to_string(), id);
        id
    }

    /// Get-or-create the shared `java/lang/Object` class id used as every
    /// other class's `super_id` (see `object_class_id` field docs).
    fn object_class(&mut self) -> u64 {
        if let Some(id) = self.object_class_id {
            return id;
        }
        let class_id = self.alloc();
        let name_id = self.intern("java/lang/Object");
        self.load_classes.extend_from_slice(&make_load_class(
            self.next_class_serial,
            class_id,
            name_id,
        ));
        self.next_class_serial += 1;
        self.class_dumps
            .extend_from_slice(&make_class_dump(class_id, 0, &[]));
        self.object_class_id = Some(class_id);
        class_id
    }

    /// Declare a class (no fields, no static anchor). `name` uses JVM
    /// internal slash notation, e.g. `"com/example/Node"`. Returns the
    /// class's object id — classes with no sticky-class root anywhere in
    /// the dump are automatically GC-rooted (MAT-parity system-class root),
    /// so this id can also be used directly as an "always reachable" anchor.
    pub fn class(&mut self, name: &str, fields: &[(&str, u8)]) -> u64 {
        if name == "java/lang/Object" && fields.is_empty() {
            return self.object_class();
        }
        let super_id = self.object_class();
        let class_id = self.alloc();
        let name_id = self.intern(name);
        self.load_classes.extend_from_slice(&make_load_class(
            self.next_class_serial,
            class_id,
            name_id,
        ));
        self.next_class_serial += 1;
        let field_ids: Vec<(u64, u8)> = fields.iter().map(|&(n, t)| (self.intern(n), t)).collect();
        self.class_dumps
            .extend_from_slice(&make_class_dump(class_id, super_id, &field_ids));
        class_id
    }

    /// Declare a class with one `Object`-typed static field pointing at
    /// `static_val` — the primary way scenarios anchor a graph without an
    /// explicit thread/JNI root. See `make_class_dump_with_static` docs.
    pub fn class_with_static_root(
        &mut self,
        name: &str,
        static_field_name: &str,
        static_val: u64,
        fields: &[(&str, u8)],
    ) -> u64 {
        let super_id = self.object_class();
        let class_id = self.alloc();
        let name_id = self.intern(name);
        self.load_classes.extend_from_slice(&make_load_class(
            self.next_class_serial,
            class_id,
            name_id,
        ));
        self.next_class_serial += 1;
        let static_name_id = self.intern(static_field_name);
        let field_ids: Vec<(u64, u8)> = fields.iter().map(|&(n, t)| (self.intern(n), t)).collect();
        self.class_dumps
            .extend_from_slice(&make_class_dump_with_static(
                class_id,
                super_id,
                static_name_id,
                static_val,
                &field_ids,
            ));
        class_id
    }

    /// Create an instance of `class_id` whose declared fields (in the exact
    /// order passed to `class()`/`class_with_static_root()`) take the given
    /// raw byte values — object fields are 8-byte big-endian ids (0 = null),
    /// ints are 4-byte big-endian. Use `obj_field`/`int_field` helpers below
    /// to build `data` without hand-counting byte offsets.
    pub fn instance(&mut self, class_id: u64, data: &[u8]) -> u64 {
        let obj_id = self.alloc();
        self.heap_subrecords
            .extend_from_slice(&make_instance_dump(obj_id, class_id, data));
        obj_id
    }

    /// Create an instance at a caller-chosen id (needed when two instances
    /// must reference each other and one must exist before the other, e.g. a
    /// doubly-linked structure). Caller is responsible for id uniqueness —
    /// use `fresh_id()` to get one.
    pub fn instance_at(&mut self, obj_id: u64, class_id: u64, data: &[u8]) {
        self.heap_subrecords
            .extend_from_slice(&make_instance_dump(obj_id, class_id, data));
    }

    /// Reserve an id without emitting any record yet (for forward references).
    pub fn fresh_id(&mut self) -> u64 {
        self.alloc()
    }

    /// Create an Object[] array (e.g. an `ArrayList`'s `elementData` backing
    /// store) with the given element class and element ids (0-length for an
    /// "empty but pre-sized" array — HPROF's OBJ_ARRAY_DUMP count IS the
    /// capacity, independent of how many elements are non-null, so a
    /// capacity-N empty-array waste scenario is simply `elem_ids` of length N
    /// filled with 0).
    pub fn obj_array(&mut self, elem_class_id: u64, elem_ids: &[u64]) -> u64 {
        let obj_id = self.alloc();
        self.heap_subrecords
            .extend_from_slice(&make_obj_array(obj_id, elem_class_id, elem_ids));
        obj_id
    }

    /// Add a `ROOT_THREAD_OBJ` GC root for an existing object id (the object
    /// must already have been created via `instance`/`instance_at`), with a
    /// fresh thread_serial. Returns the thread_serial used.
    pub fn thread_root(&mut self, thread_obj_id: u64) -> u32 {
        static NEXT_SERIAL: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);
        let serial = NEXT_SERIAL.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.heap_subrecords
            .extend_from_slice(&make_root_thread_obj(thread_obj_id, serial));
        serial
    }

    /// Finalize into raw `.hprof` bytes.
    pub fn build(self) -> Vec<u8> {
        let mut out = hprof_header();
        for (id, text) in &self.strings {
            out.extend_from_slice(&make_string(*id, text));
        }
        out.extend_from_slice(&self.load_classes);
        let mut heap = Vec::new();
        heap.extend_from_slice(&self.class_dumps);
        heap.extend_from_slice(&self.heap_subrecords);
        out.extend_from_slice(&make_heap_dump(heap));
        out
    }
}

/// Append an 8-byte big-endian object-reference field value (0 = null).
pub fn obj_field(buf: &mut Vec<u8>, id: u64) {
    buf.extend_from_slice(&id.to_be_bytes());
}

/// Append a 4-byte big-endian int field value.
pub fn int_field(buf: &mut Vec<u8>, v: i32) {
    buf.extend_from_slice(&v.to_be_bytes());
}

// ---------------------------------------------------------------------------
// Scenario-specific assemblers
// ---------------------------------------------------------------------------

/// Build a self-referential tree: `class_name` has one field `child_field`
/// (Object, pointing at another instance of the same class) and one field
/// `payload_field` (a `byte[]`-sized-via-padding int, just to give each node
/// nonzero shallow size so the tree's retained total is meaningfully above
/// SELF_REF_MIN_RETAINED). `depth` nodes are chained root -> n1 -> n2 -> ...,
/// each also carrying `payload_bytes` of padding (an extra int[] field would
/// need its own array record; instead each node gets `payload_ints` plain
/// `int` fields inline, keeping the builder dead simple).
///
/// Anchored via a static field on a freshly-declared holder class (so this
/// is reachable without relying on any other scenario's roots).
///
/// Returns the root node's object id.
pub fn build_self_referential_tree(
    b: &mut HeapBuilder,
    class_name: &str,
    depth: usize,
    payload_ints: usize,
) -> u64 {
    let mut fields: Vec<(&str, u8)> = vec![("child", T_OBJECT)];
    for i in 0..payload_ints {
        fields.push((Box::leak(format!("pad{i}").into_boxed_str()), T_INT));
    }
    let node_class = b.class(class_name, &fields);

    // Build depth-1 leaf first (child = null), then chain upward so each
    // parent's `child` field can point at the already-built child id.
    let mut child_id: u64 = 0;
    let mut root_id = 0u64;
    for level in 0..depth {
        let obj_id = b.fresh_id();
        let mut data = Vec::new();
        obj_field(&mut data, child_id);
        for i in 0..payload_ints {
            int_field(&mut data, 0x5A5A_0000 + i as i32);
        }
        b.instance_at(obj_id, node_class, &data);
        child_id = obj_id;
        if level == depth - 1 {
            root_id = obj_id;
        }
    }

    let holder_field = "root";
    b.class_with_static_root(&format!("{class_name}Holder"), holder_field, root_id, &[]);
    root_id
}

/// Build a single `java.util.ArrayList`-shaped instance with declared fields
/// matching the real JDK layout order (`size: int` then `elementData:
/// Object[]`, per `fielddecode::builtin_coll_descs()`), backed by an
/// `Object[]` array of `capacity` null slots and zero live elements —
/// exactly the "pre-sized but empty" shape `OverCapacityCollections` and
/// `EmptyCollectionCemetery` are meant to catch. Anchored via a static root.
/// Returns the ArrayList instance's object id.
pub fn build_empty_oversized_arraylist(b: &mut HeapBuilder, capacity: usize) -> u64 {
    let object_class = b.class("java/lang/Object", &[]);
    let elem_ids = vec![0u64; capacity];
    let backing_array = b.obj_array(object_class, &elem_ids);

    let list_class = b.class(
        "java/util/ArrayList",
        &[("size", T_INT), ("elementData", T_OBJECT)],
    );
    let mut data = Vec::new();
    int_field(&mut data, 0); // size = 0 (empty)
    obj_field(&mut data, backing_array);
    let list_id = b.instance(list_class, &data);

    b.class_with_static_root("ArrayListHolder", "list", list_id, &[]);
    list_id
}

/// Build the "joint Node/Thread" scenario: a live `Thread` GC root whose
/// single `Object`-typed field transits directly to the root of a deep
/// self-referential tree (as `build_self_referential_tree`, but anchored
/// via the Thread root instead of a static field) — the Thread is the
/// nearest/only GC-root holder, so a correctly-working `primary_culprits`
/// resolution must still name the tree's class, not `Thread` itself; a
/// regressed resolution would stop at the top-level dominator (the Thread)
/// and never descend into the tree.
///
/// Returns (thread_class_name_for_assertions, tree_class_name).
pub fn build_joint_node_thread_scenario(
    b: &mut HeapBuilder,
    tree_class_name: &str,
    depth: usize,
    payload_ints: usize,
) -> (String, String) {
    let mut fields: Vec<(&str, u8)> = vec![("child", T_OBJECT)];
    for i in 0..payload_ints {
        fields.push((Box::leak(format!("pad{i}").into_boxed_str()), T_INT));
    }
    let node_class = b.class(tree_class_name, &fields);

    let mut child_id: u64 = 0;
    let mut root_id = 0u64;
    for level in 0..depth {
        let obj_id = b.fresh_id();
        let mut data = Vec::new();
        obj_field(&mut data, child_id);
        for i in 0..payload_ints {
            int_field(&mut data, 0x7B7B_0000 + i as i32);
        }
        b.instance_at(obj_id, node_class, &data);
        child_id = obj_id;
        if level == depth - 1 {
            root_id = obj_id;
        }
    }

    // Thread class: one Object field ("target") pointing at the tree root.
    let thread_class_name = "java/lang/Thread";
    let thread_class = b.class(thread_class_name, &[("target", T_OBJECT)]);
    let mut tdata = Vec::new();
    obj_field(&mut tdata, root_id);
    let thread_obj = b.instance(thread_class, &tdata);
    b.thread_root(thread_obj);

    ("Thread".to_string(), tree_class_name.replace('/', "."))
}

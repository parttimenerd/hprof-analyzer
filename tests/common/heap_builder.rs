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
pub const T_BYTE: u8 = 8;
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

    /// Declare a class with an explicit super_id rather than defaulting to
    /// `java/lang/Object`. This is necessary for scenarios that need a real
    /// inheritance chain (e.g. Object → Buffer → DirectByteBuffer, or
    /// Object → Reference → WeakReference → ThreadLocalMap$Entry).
    ///
    /// Unlike `class()`, this method does NOT special-case `"java/lang/Object"`.
    /// The caller is responsible for choosing the right super_id (use
    /// `b.class("java/lang/Object", &[])` or `b.object_class()` for the root).
    pub fn class_with_super(&mut self, name: &str, super_id: u64, fields: &[(&str, u8)]) -> u64 {
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

    /// Create a primitive array (PRIM_ARRAY_DUMP) with the given element type
    /// code and raw bytes. The `type_code` and `elems` must be consistent:
    ///   - 5 = char (2 bytes/element)
    ///   - 8 = byte (1 byte/element)
    ///   - 9 = short (2 bytes/element)
    ///   - 10 = int (4 bytes/element)
    ///   - 11 = long (8 bytes/element)
    ///
    ///   Returns the array's object id.
    pub fn prim_array(&mut self, type_code: u8, elems: &[u8]) -> u64 {
        let obj_id = self.alloc();
        let count: u32 = match type_code {
            5 | 9 => (elems.len() / 2) as u32, // char, short
            8 => elems.len() as u32,           // byte
            10 => (elems.len() / 4) as u32,    // int
            11 => (elems.len() / 8) as u32,    // long
            _ => elems.len() as u32,
        };
        let mut body = Vec::new();
        write_id(&mut body, obj_id);
        body.extend_from_slice(&0u32.to_be_bytes()); // serial
        body.extend_from_slice(&count.to_be_bytes());
        body.push(type_code);
        body.extend_from_slice(elems);
        let mut rec = vec![0x23u8]; // PRIM_ARRAY_DUMP
        rec.extend_from_slice(&body);
        self.heap_subrecords.extend_from_slice(&rec);
        obj_id
    }

    /// Add a `ROOT_STICKY_CLASS` (tag 0x05) GC root record for a class object.
    /// This makes the class reachable as a "Sticky Class" root — needed for the
    /// `static-field-anchor` triage rule, which fires only when the top suspect's
    /// `root_type_label` is "Sticky Class".
    ///
    /// Format: sub-tag 0x05 + object_id (8 bytes, ID_SIZE=8).
    pub fn root_sticky_class(&mut self, class_id: u64) {
        let mut rec = vec![0x05u8]; // ROOT_STICKY_CLASS
        write_id(&mut rec, class_id);
        self.heap_subrecords.extend_from_slice(&rec);
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

// ---------------------------------------------------------------------------
// Scenario 4: Off-heap DirectByteBuffer (fires "off-heap" triage signal)
// ---------------------------------------------------------------------------

/// Build a heap with many `java/nio/DirectByteBuffer` instances whose
/// `capacity` fields sum to well above the 64 MiB floor.
///
/// Inheritance chain: `java/lang/Object` → `java/nio/Buffer` (capacity:int)
/// → `java/nio/DirectByteBuffer` (no own fields — capacity is inherited).
///
/// The `capacity` field belongs to `Buffer`, so `field_offset` walking from
/// DirectByteBuffer to the owner class `"java/nio/Buffer"` finds it at byte
/// offset 0 (no DBB-own fields precede it).
///
/// 100 DBB instances × capacity 2_000_000 = 200 MB > 64 MiB threshold.
pub fn build_off_heap_direct_byte_buffer(b: &mut HeapBuilder) {
    let object_class = b.class("java/lang/Object", &[]);
    // Buffer declares `capacity` as an int field.
    let buffer_class = b.class_with_super("java/nio/Buffer", object_class, &[("capacity", T_INT)]);
    // DirectByteBuffer extends Buffer; no own instance fields here.
    let dbb_class = b.class_with_super("java/nio/DirectByteBuffer", buffer_class, &[]);

    // 100 instances, each with capacity = 2_000_000.
    let capacity_bytes = 2_000_000i32;
    let mut first_id: u64 = 0;
    for i in 0..100u64 {
        let mut data = Vec::new();
        int_field(&mut data, capacity_bytes);
        let id = b.instance(dbb_class, &data);
        if i == 0 {
            first_id = id;
        }
    }

    // Anchor via a static root so these instances are reachable.
    b.class_with_static_root("DbbHolder", "buffers", first_id, &[]);
}

// ---------------------------------------------------------------------------
// Scenario 5: ThreadLocal null-key leak (fires "threadlocal-leak" signal)
// ---------------------------------------------------------------------------

/// Build a heap with one `ThreadLocal$ThreadLocalMap$Entry` instance whose
/// weak-reference `referent` key is null (stale entry after the ThreadLocal
/// was GC'd).
///
/// Inheritance chain:
///   `java/lang/Object`
///     → `java/lang/ref/Reference`  (referent: Object)
///       → `java/lang/ref/WeakReference`  (no own fields)
///         → `java/lang/ThreadLocal$ThreadLocalMap$Entry`  (value: Object)
///
/// Instance data layout (subclass fields first):
///   bytes 0–7: `value` Object ref (from Entry's own field)
///   bytes 8–15: `referent` Object ref (inherited from Reference)
///
/// Wait — `field_offset` walks subclass-first. So in the instance blob:
///   Entry's own field `value` comes at offset 0 (8 bytes),
///   then WeakReference's fields (none),
///   then Reference's field `referent` at offset 8.
///
/// For a null-key stale entry: referent = 0, value = some non-null object.
pub fn build_threadlocal_null_key_leak(b: &mut HeapBuilder) {
    let object_class = b.class("java/lang/Object", &[]);

    // Reference class declares the `referent` Object field.
    let reference_class = b.class_with_super(
        "java/lang/ref/Reference",
        object_class,
        &[("referent", T_OBJECT)],
    );
    // WeakReference extends Reference (no own fields).
    let weak_ref_class = b.class_with_super("java/lang/ref/WeakReference", reference_class, &[]);
    // ThreadLocalMap$Entry extends WeakReference; has a `value` Object field.
    let entry_class = b.class_with_super(
        "java/lang/ThreadLocal$ThreadLocalMap$Entry",
        weak_ref_class,
        &[("value", T_OBJECT)],
    );

    // Create a simple "value" object for the stale entry to point at.
    let value_class = b.class("scenarios/TlValue", &[]);
    let value_obj = b.instance(value_class, &[]);

    // Create a stale entry:
    //   own field `value` at offset 0 (8 bytes, non-null → the value object)
    //   inherited `referent` at offset 8 (8 bytes, null = stale key)
    let mut data = Vec::new();
    obj_field(&mut data, value_obj); // value (offset 0)
    obj_field(&mut data, 0); // referent = null (offset 8, stale)
    let entry_id = b.instance(entry_class, &data);

    b.class_with_static_root("TlEntryHolder", "entry", entry_id, &[]);
}

// ---------------------------------------------------------------------------
// Scenario 6: Boxed-primitive bloat (fires "boxed-primitive-bloat" signal)
// ---------------------------------------------------------------------------

/// Build a heap consisting almost entirely of `java/lang/Integer` instances.
/// The rule fires when boxed primitives represent >= 5% of total_shallow,
/// so with only Integer instances and a minimal holder the ratio is near 100%.
///
/// Each Integer carries one `int` field (value). Instance size: 24 bytes
/// (16-byte Object base + 4-byte int + 4-byte alignment padding).
pub fn build_boxed_primitive_bloat(b: &mut HeapBuilder) {
    let integer_class = b.class("java/lang/Integer", &[("value", T_INT)]);

    // 500 Integer instances covering essentially the whole heap.
    let mut first_id: u64 = 0;
    for i in 0..500u64 {
        let mut data = Vec::new();
        int_field(&mut data, i as i32);
        let id = b.instance(integer_class, &data);
        if i == 0 {
            first_id = id;
        }
    }

    b.class_with_static_root("IntegerHolder", "ints", first_id, &[]);
}

// ---------------------------------------------------------------------------
// Scenario 7: Duplicate strings (fires "duplicate-strings" signal)
//             Requires AnalyzeOptions { find_duplicates: true }
// ---------------------------------------------------------------------------

/// Build a heap where many `java/lang/String` instances back the same content
/// via distinct (but content-identical) `char[]` arrays.
///
/// Java 9+ layout: String has `value: byte[]` (coder=0=LATIN1, or coder=1=UTF16)
/// and `coder: byte`. Here we use old Java-8 layout (no coder field) with
/// `value` pointing to a `char[]` (type code 5).
///
/// The duplicate-strings rule fires when `approx_wasted_bytes` exceeds the 16 MiB
/// floor OR when wasted / total_shallow >= 5%. We use the percentage path by
/// making the heap very small: 200 strings sharing identical 500-char content.
///   approx_wasted_bytes = (200 - 1) × 500 = 99,500 chars ≈ 99,500 bytes
///   prim_array shallow ≈ 24 + 500*2 = 1024 bytes per array
///   String shallow ≈ 24 bytes each
///   total_shallow ≈ 200 * (24 + 1024) ≈ 209,600 bytes
///   wasted% ≈ 99500 / 209600 ≈ 47% > 5% → fires.
pub fn build_duplicate_strings(b: &mut HeapBuilder) {
    let object_class = b.class("java/lang/Object", &[]);

    // java/lang/String with a single `value` Object ref field (old layout).
    let string_class = b.class_with_super("java/lang/String", object_class, &[("value", T_OBJECT)]);

    // 200 char[] arrays, each containing the same 500 chars (A = 0x0041).
    // char is type code 5, 2 bytes/element.
    let char_content: Vec<u8> = (0..500u32)
        .flat_map(|_| [0x00u8, 0x41u8]) // 'A' in UTF-16 BE
        .collect();

    let mut first_string_id: u64 = 0;
    for i in 0..200u64 {
        // Each string gets its OWN backing array with identical content.
        let arr_id = b.prim_array(5, &char_content); // type 5 = char

        // String instance: value field points to this array.
        let mut data = Vec::new();
        obj_field(&mut data, arr_id);
        let str_id = b.instance(string_class, &data);
        if i == 0 {
            first_string_id = str_id;
        }
    }

    b.class_with_static_root("StringHolder", "strings", first_string_id, &[]);
}

// ---------------------------------------------------------------------------
// Scenario 9: Single culprit SimpleValue class with holder data
//             Verifies that biggest_classes[0].holders is non-empty when the
//             dominant class is held by a named container class.
// ---------------------------------------------------------------------------

/// Build a heap where `scenarios/SimpleValue` is the biggest class by retained
/// size, held by a `scenarios/SimpleContainer`. A static GC-root anchors the
/// container so it is reachable.
///
/// The scenario verifies that `biggest_classes[0].holders` is populated after
/// the holder-computation window change (P0-2 fix).
pub fn build_single_culprit_simple_class(b: &mut HeapBuilder) {
    let simple_class = b.class(
        "scenarios/SimpleValue",
        &[("x", T_INT), ("y", T_INT), ("z", T_INT)],
    );

    // Create 50 SimpleValue instances (each has 3 int fields → meaningful shallow size)
    let mut refs: Vec<u64> = Vec::new();
    for _ in 0..50 {
        let mut data = Vec::new();
        int_field(&mut data, 0x1234);
        int_field(&mut data, 0x5678);
        int_field(&mut data, 0x9ABC);
        refs.push(b.instance(simple_class, &data));
    }

    // Container holds an Object[] of all SimpleValues
    let obj_class = b.class("java/lang/Object", &[]);
    let arr = b.obj_array(obj_class, &refs);
    let container_class = b.class("scenarios/SimpleContainer", &[("values", T_OBJECT)]);
    let mut cdata = Vec::new();
    obj_field(&mut cdata, arr);
    let container = b.instance(container_class, &cdata);

    // Static GC root anchors the container
    b.class_with_static_root("scenarios/Root", "container", container, &[]);
}

// ---------------------------------------------------------------------------
// Scenario 10: Healthy baseline (no triage signals expected)
// ---------------------------------------------------------------------------

/// Build a tiny, completely unremarkable heap — a single small object and
/// nothing else. No triage signal should fire (all are threshold-gated).
pub fn build_healthy_baseline(b: &mut HeapBuilder) {
    // Create multiple independent root objects of roughly equal size so no
    // single object crosses the 50% dominance floor.
    let cls = b.class("scenarios/Ping", &[("x", T_INT)]);
    for i in 0..5i32 {
        let mut data = Vec::new();
        int_field(&mut data, i);
        let obj = b.instance(cls, &data);
        b.thread_root(obj);
    }
}

// ---------------------------------------------------------------------------
// Scenario 11: Multiple ThreadLocal null-key leaks (pooled-thread pattern)
// ---------------------------------------------------------------------------

/// Build a heap with 5 `ThreadLocal$ThreadLocalMap$Entry` instances, each
/// with a null `referent` (stale key). Models a thread-pool scenario where
/// 5 pooled threads each have one stale ThreadLocal value that was never
/// removed after the ThreadLocal was GC'd.
///
/// Uses the same inheritance chain as Scenario 5. The "threadlocal-leak"
/// signal must fire when there are multiple stale entries across threads.
pub fn build_threadlocal_pooled_multiple(b: &mut HeapBuilder) {
    let object_class = b.class("java/lang/Object", &[]);
    let reference_class = b.class_with_super(
        "java/lang/ref/Reference",
        object_class,
        &[("referent", T_OBJECT)],
    );
    let weak_ref_class = b.class_with_super("java/lang/ref/WeakReference", reference_class, &[]);
    let entry_class = b.class_with_super(
        "java/lang/ThreadLocal$ThreadLocalMap$Entry",
        weak_ref_class,
        &[("value", T_OBJECT)],
    );

    let value_class = b.class("scenarios/PooledTlValue", &[]);
    // Create 5 stale entries (simulating 5 pooled threads each with one stale TL value).
    let mut first_id: u64 = 0;
    for i in 0..5u64 {
        let value_obj = b.instance(value_class, &[]);
        let mut data = Vec::new();
        obj_field(&mut data, value_obj); // value (non-null)
        obj_field(&mut data, 0); // referent = null (stale)
        let entry_id = b.instance(entry_class, &data);
        if i == 0 {
            first_id = entry_id;
        }
    }
    b.class_with_static_root("PoolTlHolder", "entry", first_id, &[]);
}

/// table and zero live entries — exactly the "pre-sized but empty" shape that
/// triggers `OverCapacityCollections`.
///
/// HashMap layout (from `builtin_coll_descs()`):
///   `size` (int, field in java/util/HashMap)
///   `table` (Object ref → the backing Object[] array, field in java/util/HashMap)
pub fn build_over_capacity_hashmap(b: &mut HeapBuilder) -> u64 {
    let object_class = b.class("java/lang/Object", &[]);
    let elem_ids = vec![0u64; 200_000];
    let backing_array = b.obj_array(object_class, &elem_ids);

    let map_class = b.class("java/util/HashMap", &[("size", T_INT), ("table", T_OBJECT)]);
    let mut data = Vec::new();
    int_field(&mut data, 0); // size = 0 (empty)
    obj_field(&mut data, backing_array); // table → the backing array
    let map_id = b.instance(map_class, &data);

    b.class_with_static_root("HashMapHolder", "map", map_id, &[]);
    map_id
}

// ---------------------------------------------------------------------------
// Scenario 12: Static-field anchor (fires "static-field-anchor" signal)
// ---------------------------------------------------------------------------

/// Build a heap where a class is itself a `ROOT_STICKY_CLASS` GC root and retains
/// many objects via a static field. The `static-field-anchor` triage rule fires
/// when the top suspect's `root_type_label` is "Sticky Class" and it retains
/// >= 20% of the heap.
///
/// We emit an explicit ROOT_STICKY_CLASS (0x05) record for the holder class
/// so pass2 records that class's GC root type as "Sticky Class". The holder
/// class then holds a chain of nodes through its static field, so the class
/// (as a top-level dominator) accumulates substantial retained size.
///
/// The class itself is the suspect here (not its instances) — class objects
/// can be GC roots and suspects when they dominate large subgraphs.
pub fn build_static_field_anchor(b: &mut HeapBuilder) {
    // Node class: each instance has one int field for size.
    let node_class = b.class("scenarios/CachedEntry", &[("value", T_INT)]);

    // Build 100 nodes — enough retained to dominate the heap.
    let mut refs: Vec<u64> = Vec::new();
    for i in 0..100i32 {
        let mut data = Vec::new();
        int_field(&mut data, i);
        refs.push(b.instance(node_class, &data));
    }

    // Object[] backing array holds all nodes.
    let object_class = b.class("java/lang/Object", &[]);
    let arr = b.obj_array(object_class, &refs);

    // Cache class: static field → array. We explicitly emit ROOT_STICKY_CLASS
    // for this class so it has root_type_label == "Sticky Class".
    let cache_class = b.class_with_static_root("scenarios/AppCache", "entries", arr, &[]);
    b.root_sticky_class(cache_class);
}

// ---------------------------------------------------------------------------
// Scenario 13: Session-scope leak (fires "session-scope-leak" signal)
// ---------------------------------------------------------------------------

/// Build a heap with >= 100,000 instances of a class whose name contains "Session".
/// The `session-scope-leak` rule fires on name-pattern match (>= SESSION_FLOOR = 100,000).
///
/// All instances are anchored in an Object[] held by a static root so they are
/// counted as live objects in the histogram.
pub fn build_session_scope_leak(b: &mut HeapBuilder) {
    // UserSession class: two int fields to give each instance nonzero size.
    let session_class = b.class("com/example/UserSession", &[("id", T_INT), ("ts", T_INT)]);

    let mut session_ids: Vec<u64> = Vec::with_capacity(100_000);
    for i in 0..100_000u64 {
        let mut data = Vec::new();
        int_field(&mut data, i as i32);
        int_field(&mut data, (i * 13) as i32);
        session_ids.push(b.instance(session_class, &data));
    }

    // Anchor all sessions via a static field holding an Object[] array.
    let object_class = b.class("java/lang/Object", &[]);
    let arr = b.obj_array(object_class, &session_ids);
    b.class_with_static_root("scenarios/SessionRegistry", "sessions", arr, &[]);
}

// ---------------------------------------------------------------------------
// Scenario 14: Unique strings — no duplicate-strings signal
//              (false-positive guard: must-not-fire test)
// ---------------------------------------------------------------------------

/// Build a heap with 50 strings whose backing char arrays all have UNIQUE
/// content. The `duplicate-strings` rule must NOT fire (each string has a
/// different sequence of chars → no shared content → wasted% = 0).
///
/// Requires AnalyzeOptions { find_duplicates: true } to enable the scan.
pub fn build_unique_strings_no_dup(b: &mut HeapBuilder) {
    let object_class = b.class("java/lang/Object", &[]);
    let string_class = b.class_with_super("java/lang/String", object_class, &[("value", T_OBJECT)]);

    let mut first_id: u64 = 0;
    for i in 0u32..50 {
        // Each backing array has unique content: 10 chars spelling out the index.
        // Encode i as 10 BE char values 0x0030..0x0039 (ASCII digits).
        let digits: Vec<u8> = format!("{i:010}")
            .chars()
            .flat_map(|c| [(c as u16).to_be_bytes()[0], (c as u16).to_be_bytes()[1]])
            .collect();
        let arr_id = b.prim_array(5, &digits); // type 5 = char
        let mut data = Vec::new();
        obj_field(&mut data, arr_id);
        let str_id = b.instance(string_class, &data);
        if i == 0 {
            first_id = str_id;
        }
    }
    b.class_with_static_root("UniqueStringHolder", "strings", first_id, &[]);
}

// ---------------------------------------------------------------------------
// Scenario 16: Single large object with static-field ownership via root_path
//              Verifies static-field-anchor and concentration name the holder
// ---------------------------------------------------------------------------

/// Build a heap with ONE big `DataBuffer` instance (holding a 200 KB byte[])
/// anchored by a static field on `StaticCacheService`. The DataBuffer is the
/// primary culprit as a single object. `static-field-anchor` must name
/// `StaticCacheService` as the holder; `concentration` should too (via
/// root_path attribution, no --collections required).
///
/// Shape:
///   StaticCacheService [Sticky Class] --static--> DataBuffer --field--> byte[200KB]
pub fn build_single_object_static_owner(b: &mut HeapBuilder) {
    let object_class = b.class("java/lang/Object", &[]);
    // DataBuffer has one Object field (the backing byte[]).
    let data_buffer_class =
        b.class_with_super("scenarios/DataBuffer", object_class, &[("data", T_OBJECT)]);

    // Build the large backing array (~200 KB).
    let big_array = b.prim_array(8, &vec![42u8; 200_000]); // byte[] 200 KB

    // Build ONE DataBuffer instance pointing to the array.
    let mut inst_data = Vec::new();
    obj_field(&mut inst_data, big_array);
    let buffer_id = b.instance(data_buffer_class, &inst_data);

    // StaticCacheService holds the DataBuffer via a static field.
    let holder = b.class_with_static_root("scenarios/StaticCacheService", "cache", buffer_id, &[]);
    b.root_sticky_class(holder);
}

// ---------------------------------------------------------------------------
// Scenario 17: LinkedList accumulation — deep-retention-chain
//              A chain of 1,200 nodes that crosses the 1,000-hop threshold.
// ---------------------------------------------------------------------------

/// Build a singly-linked chain of 1,200 `QueueNode` objects, each holding a
/// small 512-byte payload. The chain is anchored by a static field on
/// `UnboundedQueue`. The dominator tree's longest chain will be >= 1,200 —
/// well above the `DEEP_CHAIN_DEPTH` threshold of 1,000.
///
/// Shape:
///   UnboundedQueue [Sticky Class] --static--> node1 --> node2 --> ... --> node1200
pub fn build_linked_list_chain(b: &mut HeapBuilder) {
    // QueueNode has one Object field for the next pointer.
    let node_class = b.class("scenarios/QueueNode", &[("next", T_OBJECT)]);

    // Build leaf-to-root so each parent can point at the already-created child.
    let mut prev_id: u64 = 0;
    let mut head_id: u64 = 0;
    for i in 0..1_200usize {
        let obj_id = b.fresh_id();
        let mut data = Vec::new();
        obj_field(&mut data, prev_id); // next = previous (so root is built last)
        b.instance_at(obj_id, node_class, &data);
        prev_id = obj_id;
        if i == 1_199 {
            head_id = obj_id;
        }
    }

    // Anchor the head via a static field.
    let holder = b.class_with_static_root("scenarios/UnboundedQueue", "head", head_id, &[]);
    b.root_sticky_class(holder);
}

// ---------------------------------------------------------------------------
// Scenario 18: sankey_imdom_chain — ImmDomPair two-hop chain reproduced
// ---------------------------------------------------------------------------

/// Build a three-level ownership chain for the "Who Holds This Class?" Sankey
/// reproducer:
///
///   Container (static root) --field--> Substatement --field--> TargetClass
///
/// Each Container holds exactly one Substatement; each Substatement holds
/// exactly one TargetClass. With N=50 triples, the report's ImmDomPair table
/// must contain:
///   - (Substatement → TargetClass): Substatement is the *immediate* dominator
///     of all TargetClass objects.
///   - (Container → Substatement): Container is the immediate dominator of all
///     Substatement objects.
///
/// This verifies that the Sankey's "expand left" step — filtering pairs by
/// `dominated_class === target` — will find the correct intermediate class
/// (Substatement) as a holder of TargetClass when the dominator relationship
/// is genuinely direct.
pub fn build_sankey_imdom_chain(b: &mut HeapBuilder, n: usize) {
    // TargetClass: the leaf class (what the Sankey is centred on).
    let target_class = b.class("scenarios/TargetClass", &[("v", T_INT)]);
    // Substatement: intermediate holder with one field pointing at a TargetClass.
    let sub_class = b.class("scenarios/Substatement", &[("target", T_OBJECT)]);
    // Container: top-level holder — holds a Substatement + next Container
    // (forms a singly-linked list so all N containers are reachable from
    // the static root without needing an explicit array record).
    let container_class = b.class(
        "scenarios/Container",
        &[("sub", T_OBJECT), ("next", T_OBJECT)],
    );

    // Build leaf-to-root: create leaves first, then link containers back.
    let mut prev_container: u64 = 0;
    let mut head_container: u64 = 0;
    for i in 0..n {
        // Leaf
        let mut d = Vec::new();
        int_field(&mut d, i as i32);
        let target_id = b.instance(target_class, &d);

        // Substatement → TargetClass
        let mut d = Vec::new();
        obj_field(&mut d, target_id);
        let sub_id = b.instance(sub_class, &d);

        // Container → Substatement, Container.next → previous container
        let mut d = Vec::new();
        obj_field(&mut d, sub_id);
        obj_field(&mut d, prev_container);
        let container_id = b.instance(container_class, &d);

        prev_container = container_id;
        head_container = container_id;
    }

    // Anchor the head (last-created, with the longest .next chain) via a
    // static field so all containers — and hence all Substatements and
    // TargetClass objects — are reachable from the GC root.
    b.class_with_static_root("scenarios/ChainRoot", "root", head_container, &[]);
}

/// Build a heap consisting almost entirely of zero-filled `byte[]` arrays large
/// enough to clear CONSTARR_FLOOR (8 MiB total shallow). The
/// `constant-value-arrays` rule fires when constant-valued primitive arrays
/// (all elements the same byte) sum to >= 8 MiB.
///
/// 100 arrays × 100,000 bytes each = 10 MB > 8 MB floor → fires.
pub fn build_constant_value_arrays(b: &mut HeapBuilder) {
    let zero_elem = vec![0u8; 100_000]; // 100 KB of zero bytes per array
    let mut first_id: u64 = 0;
    for i in 0..100u64 {
        let arr_id = b.prim_array(8, &zero_elem); // type 8 = byte
        if i == 0 {
            first_id = arr_id;
        }
    }
    b.class_with_static_root("ConstArrayHolder", "arrays", first_id, &[]);
}

/// Build a heap that exercises two-hop holder detection in `build_top_consumers`.
///
/// Chain: `Container → Leaf[]` (obj array) → `Leaf`
///
/// `Leaf`'s direct (level-1) holder is the `Leaf[]` array class.
/// `Container` holds the array, so it is the two-hop holder of `Leaf` and
/// must appear in `biggest_classes[Leaf].holders` with `via_class = "Leaf[]"`.
pub fn build_two_hop_holders(b: &mut HeapBuilder, n: usize) {
    let leaf_class = b.class("scenarios/Leaf", &[("v", T_INT)]);
    let container_class = b.class("scenarios/Container", &[("arr", T_OBJECT)]);

    // n Leaf instances, all held in one Container's array
    let mut leaf_ids: Vec<u64> = Vec::with_capacity(n);
    for i in 0..n {
        let mut d = Vec::new();
        int_field(&mut d, i as i32);
        leaf_ids.push(b.instance(leaf_class, &d));
    }
    let arr_id = b.obj_array(leaf_class, &leaf_ids);
    let mut d = Vec::new();
    obj_field(&mut d, arr_id);
    let container_id = b.instance(container_class, &d);
    b.class_with_static_root("scenarios/TwoHopRoot", "root", container_id, &[]);
}

// ---------------------------------------------------------------------------
// Scenario 19: category_in_histogram — category is non-empty on ClassRow
// ---------------------------------------------------------------------------

/// Build a heap with many `com.example.Widget` instances so `biggest_classes`
/// is non-empty and the `category` field is populated. Uses the same pattern
/// as `build_single_culprit_simple_class` (50 instances with int fields) but
/// with a `com.example` class to test the "other" category bucket.
pub fn build_app_class_in_histogram() -> Vec<u8> {
    let mut b = HeapBuilder::new();
    // 50 instances of com.example.Widget with 3 int fields each —
    // same pattern as build_single_culprit_simple_class, which reliably
    // produces biggest_classes entries.
    let cls = b.class(
        "com/example/Widget",
        &[("x", T_INT), ("y", T_INT), ("z", T_INT)],
    );
    let mut refs: Vec<u64> = Vec::new();
    for i in 0..50i32 {
        let mut data = Vec::new();
        int_field(&mut data, i);
        int_field(&mut data, i * 2);
        int_field(&mut data, i * 3);
        refs.push(b.instance(cls, &data));
    }
    let obj_class = b.class("java/lang/Object", &[]);
    let arr = b.obj_array(obj_class, &refs);
    let container_class = b.class("com/example/WidgetRegistry", &[("widgets", T_OBJECT)]);
    let mut cdata = Vec::new();
    obj_field(&mut cdata, arr);
    let container = b.instance(container_class, &cdata);
    b.class_with_static_root("com/example/AppRoot", "registry", container, &[]);
    b.build()
}

// ---------------------------------------------------------------------------
// Scenario 20: culprit_prefers_app_class_over_jdk
// ---------------------------------------------------------------------------

/// Build a heap where a `com.example.Cache` instance is a DIRECT top-level
/// dominator (via a Thread GC root), retaining 200 byte arrays that each have
/// 4096 bytes. The com.example.Cache must be nominated as primary culprit.
///
/// The Cache is the thread's `target` field, making it a direct child of the
/// thread in the dominator tree. The thread itself is the GC root but the
/// Cache is the real accumulator. The primary_culprit path resolution must
/// pick Cache (the app class), not Thread.
pub fn build_app_class_culprit() -> Vec<u8> {
    let mut b = HeapBuilder::new();

    // Build 200 byte arrays, each 4096 bytes.
    let mut payload_ids: Vec<u64> = Vec::with_capacity(200);
    for _ in 0..200 {
        payload_ids.push(b.prim_array(T_BYTE, &vec![0u8; 4096]));
    }

    // A simple obj array to hold the payloads.
    let object_class = b.class("java/lang/Object", &[]);
    let arr_obj = b.obj_array(object_class, &payload_ids);

    // Cache class: one Object field (the array).
    let cache_cls = b.class("com/example/Cache", &[("data", T_OBJECT)]);
    let mut buf = Vec::new();
    obj_field(&mut buf, arr_obj);
    let cache_obj = b.instance(cache_cls, &buf);

    // Thread class: one Object field ("target") pointing at Cache.
    let thread_class = b.class("java/lang/Thread", &[("target", T_OBJECT)]);
    let mut tdata = Vec::new();
    obj_field(&mut tdata, cache_obj);
    let thread_obj = b.instance(thread_class, &tdata);
    b.thread_root(thread_obj);

    b.build()
}

// ---------------------------------------------------------------------------
// Scenario 21: subclass_hierarchy_no_panic (class_with_super coverage)
// ---------------------------------------------------------------------------

/// Build a heap with a three-level inheritance hierarchy (Base → Sub → SubSub)
/// to verify that `class_with_super` chains are handled without panic in all
/// analysis passes (field offset walks, dominator computation, etc.).
pub fn build_subclass_hierarchy() -> Vec<u8> {
    let mut b = HeapBuilder::new();
    let base = b.class("com/example/Base", &[("x", T_INT)]);
    let sub = b.class_with_super("com/example/Sub", base, &[("y", T_INT)]);
    let subsub = b.class_with_super("com/example/SubSub", sub, &[("z", T_INT)]);
    // SubSub instance data only carries its own declared field (z).
    let arr = b.prim_array(T_BYTE, &vec![0u8; 256 * 1024]);
    let mut buf = Vec::new();
    int_field(&mut buf, 1); // z field value
    let obj = b.instance(subsub, &buf);
    // Anchor via a static root and add sticky-class roots for all three classes.
    b.class_with_static_root("com/example/Root", "val", obj, &[]);
    b.root_sticky_class(base);
    b.root_sticky_class(sub);
    b.root_sticky_class(subsub);
    // Add a large prim array to push retained size high enough for the
    // subclass object to appear as a significant suspect.
    let _ = arr; // arr is an independent root, not wired here — that's fine for a no-panic check
    b.build()
}

// ---------------------------------------------------------------------------
// Scenario 22: mixed_class_heap (classof() coverage, no-panic)
// ---------------------------------------------------------------------------

/// Build a heap with multiple instance types so that any code path that
/// iterates over instances and calls classof() must handle heterogeneous
/// classes without panicking.
pub fn build_mixed_class_heap() -> Vec<u8> {
    let mut b = HeapBuilder::new();
    let a = b.class("com/example/TypeA", &[("x", T_INT)]);
    let c = b.class("com/example/TypeB", &[("y", T_INT)]);
    for _ in 0..5 {
        let mut v = Vec::new();
        int_field(&mut v, 1);
        b.instance(a, &v);
        let mut v = Vec::new();
        int_field(&mut v, 2);
        b.instance(c, &v);
    }
    b.root_sticky_class(a);
    b.root_sticky_class(c);
    b.build()
}

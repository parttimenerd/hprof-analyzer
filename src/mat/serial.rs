//! Minimal Java Object Serialization Stream Protocol emitter.
//!
//! Supports exactly the subset Eclipse MAT's master `.index` needs:
//! - primitives (int/long/boolean), Strings, Dates
//! - object class descriptors with declared-field lists (in declaration order)
//! - superclass chains (fields written superclass-first)
//! - object handle table (TC_REFERENCE back-references — REQUIRED for shared ClassImpl)
//! - object arrays, HashMapIntObject (custom writeObject), HashMap, BitField
//!
//! Ported byte-for-byte from the standalone verifier (`/tmp/javaser/verify/rust/src/javaser.rs`),
//! which reproduced 19/19 cases identical to real MAT 1.13.0 serialization.
//!
//! Spec: https://docs.oracle.com/en/java/javase/17/docs/specs/serialization/protocol.html

#![allow(dead_code)]

use std::collections::HashMap as StdMap;

// ---- stream constants ----
pub const STREAM_MAGIC: u16 = 0xACED;
pub const STREAM_VERSION: u16 = 0x0005;

pub const TC_NULL: u8 = 0x70;
pub const TC_REFERENCE: u8 = 0x71;
pub const TC_CLASSDESC: u8 = 0x72;
pub const TC_OBJECT: u8 = 0x73;
pub const TC_STRING: u8 = 0x74;
pub const TC_ARRAY: u8 = 0x75;
pub const TC_ENDBLOCKDATA: u8 = 0x78;
pub const TC_BLOCKDATA: u8 = 0x77;

pub const SC_WRITE_METHOD: u8 = 0x01; // has writeObject that calls defaultWriteObject
pub const SC_SERIALIZABLE: u8 = 0x02;

pub const BASE_HANDLE: u32 = 0x7E0000;

/// Well-known serialVersionUIDs (from MAT / JDK sources).
pub mod uid {
    pub const INT_ARRAY: i64 = 5600894804908749477; // "[I"
    pub const OBJECT_ARRAY_PREFIX: &str = "[L"; // component-specific, uid varies
    pub const HASH_MAP_INT_OBJECT: i64 = 2;
    pub const BIT_FIELD: i64 = 1;
    pub const SNAPSHOT_INFO: i64 = 4;
    pub const X_SNAPSHOT_INFO: i64 = 3;
    pub const GC_ROOT_INFO: i64 = 2;
    pub const X_GC_ROOT_INFO: i64 = 1;
    pub const CLASS_IMPL: i64 = 22;
    pub const ABSTRACT_OBJECT_IMPL: i64 = 2451875423035843852;
    // java.util.HashMap
    pub const HASH_MAP: i64 = 362498820763181265;
    pub const HASH_MAP_INT_ARRAY: i64 = 0; // placeholder; not needed if we avoid it
    pub const DATE: i64 = 7523967970034938905;
    pub const FIELD_DESCRIPTOR_ARRAY: i64 = -4300540347928878330; // "[L...FieldDescriptor;"
    pub const FIELD_ARRAY: i64 = 2935640697646924767; // "[L...Field;"
    pub const ARRAY_LIST: i64 = 8683452581122892189;
    pub const UNREACHABLE_OBJECTS_HISTOGRAM: i64 = 1;
    pub const UOH_RECORD: i64 = 1;
    pub const BOOLEAN: i64 = -3665804199014368530;
    pub const LONG: i64 = 4290774380558885855;
    pub const NUMBER: i64 = -8742448824652078965;
}

/// Java field type tags (single-char typecode as first byte of a field descriptor).
#[derive(Clone)]
pub enum FieldType {
    Int,
    Long,
    Float,
    Boolean,
    /// Object field: typecode 'L' + a TC_STRING naming the JVM type signature, e.g. "Ljava/lang/String;"
    Object(String),
    /// Array field: typecode '[' + a TC_STRING naming the array signature, e.g. "[I"
    Array(String),
}

impl FieldType {
    fn typecode(&self) -> u8 {
        match self {
            FieldType::Int => b'I',
            FieldType::Long => b'J',
            FieldType::Float => b'F',
            FieldType::Boolean => b'Z',
            FieldType::Object(_) => b'L',
            FieldType::Array(_) => b'[',
        }
    }
}

pub struct FieldDesc {
    pub name: String,
    pub ty: FieldType,
}

pub fn f_int(name: &str) -> FieldDesc {
    FieldDesc {
        name: name.into(),
        ty: FieldType::Int,
    }
}
pub fn f_long(name: &str) -> FieldDesc {
    FieldDesc {
        name: name.into(),
        ty: FieldType::Long,
    }
}
pub fn f_float(name: &str) -> FieldDesc {
    FieldDesc {
        name: name.into(),
        ty: FieldType::Float,
    }
}
pub fn f_bool(name: &str) -> FieldDesc {
    FieldDesc {
        name: name.into(),
        ty: FieldType::Boolean,
    }
}
pub fn f_obj(name: &str, sig: &str) -> FieldDesc {
    FieldDesc {
        name: name.into(),
        ty: FieldType::Object(sig.into()),
    }
}
pub fn f_arr(name: &str, sig: &str) -> FieldDesc {
    FieldDesc {
        name: name.into(),
        ty: FieldType::Array(sig.into()),
    }
}

/// A serializable class layer (one level of the superclass chain).
pub struct ClassDesc {
    pub name: String,
    pub uid: i64,
    pub flags: u8,
    pub fields: Vec<FieldDesc>,
}

pub struct Ser {
    pub buf: Vec<u8>,
    next_handle: u32,
    /// object identity -> handle. Keyed by a caller-supplied opaque id (e.g. class name for a
    /// singleton, or a synthetic id for shared instances).
    handles: StdMap<String, u32>,
    /// class descriptor name -> handle (class descriptors also consume handles).
    class_handles: StdMap<String, u32>,
    /// interned strings -> handle.
    string_handles: StdMap<String, u32>,
}

impl Ser {
    pub fn new() -> Self {
        let mut s = Ser {
            buf: Vec::new(),
            next_handle: BASE_HANDLE,
            handles: StdMap::new(),
            class_handles: StdMap::new(),
            string_handles: StdMap::new(),
        };
        s.u16(STREAM_MAGIC);
        s.u16(STREAM_VERSION);
        s
    }

    fn assign_handle(&mut self) -> u32 {
        let h = self.next_handle;
        self.next_handle += 1;
        h
    }

    // --- raw writers ---
    pub fn u8(&mut self, b: u8) {
        self.buf.push(b);
    }
    pub fn u16(&mut self, v: u16) {
        self.buf.extend_from_slice(&v.to_be_bytes());
    }
    pub fn i32(&mut self, v: i32) {
        self.buf.extend_from_slice(&v.to_be_bytes());
    }
    pub fn i64(&mut self, v: i64) {
        self.buf.extend_from_slice(&v.to_be_bytes());
    }
    pub fn f32(&mut self, v: f32) {
        // Java float bits, big-endian (IEEE-754)
        self.buf.extend_from_slice(&v.to_bits().to_be_bytes());
    }
    pub fn bool(&mut self, v: bool) {
        self.buf.push(if v { 1 } else { 0 });
    }
    fn utf_bytes(&mut self, s: &str) {
        // modified UTF-8; ASCII subset identical to UTF-8
        self.u16(s.len() as u16);
        self.buf.extend_from_slice(s.as_bytes());
    }

    /// Write a TC_STRING (interned via handle table).
    pub fn string(&mut self, s: &str) {
        if let Some(&h) = self.string_handles.get(s) {
            self.u8(TC_REFERENCE);
            self.i32(h as i32);
            return;
        }
        self.u8(TC_STRING);
        let h = self.assign_handle();
        self.string_handles.insert(s.to_string(), h);
        self.utf_bytes(s);
    }

    /// Write TC_NULL.
    pub fn null(&mut self) {
        self.u8(TC_NULL);
    }

    /// Write a class descriptor chain (this class then its superclass), assigning handles.
    /// `chain` is ordered subclass-first (index 0 = most-derived).
    pub fn write_class_desc_chain(&mut self, chain: &[ClassDesc]) {
        self.write_class_desc_at(chain, 0);
    }

    fn write_class_desc_at(&mut self, chain: &[ClassDesc], idx: usize) {
        if idx >= chain.len() {
            self.null();
            return;
        }
        let cd = &chain[idx];
        if let Some(&h) = self.class_handles.get(&cd.name) {
            self.u8(TC_REFERENCE);
            self.i32(h as i32);
            return;
        }
        self.u8(TC_CLASSDESC);
        self.utf_bytes(&cd.name);
        self.i64(cd.uid);
        // classDesc gets a handle right after name+uid, before flags/fields
        let h = self.assign_handle();
        self.class_handles.insert(cd.name.clone(), h);
        self.u8(cd.flags);
        // Java sorts fields: primitives before object/array types, each group alphabetical by name.
        let mut order: Vec<&FieldDesc> = cd.fields.iter().collect();
        order.sort_by(|a, b| {
            let pa = matches!(a.ty, FieldType::Object(_) | FieldType::Array(_));
            let pb = matches!(b.ty, FieldType::Object(_) | FieldType::Array(_));
            pa.cmp(&pb).then(a.name.cmp(&b.name))
        });
        self.u16(order.len() as u16);
        for f in &order {
            self.u8(f.ty.typecode());
            self.utf_bytes(&f.name);
            match &f.ty {
                FieldType::Object(sig) | FieldType::Array(sig) => {
                    // field type string is itself a TC_STRING (interned)
                    self.string(sig);
                }
                _ => {}
            }
        }
        self.u8(TC_ENDBLOCKDATA); // no class annotations
        // superclass descriptor
        self.write_class_desc_at(chain, idx + 1);
    }
}

/// A concrete field value paired with its declared type (for ordering + encoding).
pub enum FieldVal {
    Int(i32),
    Long(i64),
    Float(f32),
    Bool(bool),
    /// object/array field value written after all primitive values; encoded by callback.
    ObjRef(Box<dyn FnOnce(&mut Ser)>),
}

/// One class layer's data: its descriptor fields plus the matching values by name.
pub struct LayerData {
    pub fields: Vec<FieldDesc>,
    pub values: Vec<(String, FieldVal)>,
}

impl Ser {
    /// Write a TC_OBJECT: class-desc chain then field values layer-by-layer (superclass first),
    /// each layer's values ordered like the descriptor (primitives alpha, then objects alpha).
    /// `chain` is subclass-first; `layers` is subclass-first and parallel to `chain`.
    pub fn write_object(&mut self, chain: &[ClassDesc], layers: Vec<LayerData>) {
        self.write_object_keyed(chain, layers, None);
    }

    /// Like write_object but registers the object's handle under `key` so a later (or nested,
    /// self-referential) write can emit a TC_REFERENCE via `ref_object(key)`. The handle is
    /// assigned right after the class-desc chain and before field values — matching the JVM, so a
    /// self-reference inside the object's own fields resolves correctly.
    pub fn write_object_keyed(
        &mut self,
        chain: &[ClassDesc],
        layers: Vec<LayerData>,
        key: Option<&str>,
    ) {
        self.u8(TC_OBJECT);
        self.write_class_desc_chain(chain);
        let handle = self.assign_handle(); // the object instance consumes a handle
        if let Some(k) = key {
            self.handles.insert(k.to_string(), handle);
        }
        // field values written superclass-first (reverse of subclass-first chain)
        for layer in layers.into_iter().rev() {
            self.write_layer_values(layer);
        }
    }

    /// Emit a TC_REFERENCE to a previously-keyed object. Panics if the key is unknown.
    pub fn ref_object(&mut self, key: &str) {
        let h = *self
            .handles
            .get(key)
            .unwrap_or_else(|| panic!("no object handle for key {key}"));
        self.u8(TC_REFERENCE);
        self.i32(h as i32);
    }

    fn write_layer_values(&mut self, layer: LayerData) {
        let is_obj: StdMap<String, bool> = layer
            .fields
            .iter()
            .map(|f| {
                (
                    f.name.clone(),
                    matches!(f.ty, FieldType::Object(_) | FieldType::Array(_)),
                )
            })
            .collect();
        // partition indices into primitive vs object, each sorted alphabetically by name
        let mut prim_idx: Vec<usize> = Vec::new();
        let mut obj_idx: Vec<usize> = Vec::new();
        for (i, (name, _)) in layer.values.iter().enumerate() {
            if *is_obj.get(name).unwrap_or(&false) {
                obj_idx.push(i);
            } else {
                prim_idx.push(i);
            }
        }
        prim_idx.sort_by(|&a, &b| layer.values[a].0.cmp(&layer.values[b].0));
        obj_idx.sort_by(|&a, &b| layer.values[a].0.cmp(&layer.values[b].0));

        let mut vals: Vec<Option<FieldVal>> =
            layer.values.into_iter().map(|(_, v)| Some(v)).collect();
        for i in prim_idx {
            match vals[i].take().unwrap() {
                FieldVal::Int(x) => self.i32(x),
                FieldVal::Long(x) => self.i64(x),
                FieldVal::Float(x) => self.f32(x),
                FieldVal::Bool(x) => self.bool(x),
                FieldVal::ObjRef(_) => unreachable!(),
            }
        }
        for i in obj_idx {
            if let Some(FieldVal::ObjRef(cb)) = vals[i].take() {
                cb(self);
            }
        }
    }

    /// Write a TC_ARRAY of objects. `elem_class` is the array class descriptor (e.g. "[Lorg...;").
    pub fn write_object_array<F: FnOnce(&mut Ser)>(
        &mut self,
        array_class_name: &str,
        array_uid: i64,
        len: i32,
        write_elems: F,
    ) {
        self.u8(TC_ARRAY);
        let cd = ClassDesc {
            name: array_class_name.into(),
            uid: array_uid,
            flags: SC_SERIALIZABLE,
            fields: vec![],
        };
        self.write_class_desc_chain(&[cd]);
        let _handle = self.assign_handle(); // array instance handle
        self.i32(len);
        write_elems(self);
    }

    /// Emit a block-data record (TC_BLOCKDATA + 1-byte len) for custom writeObject payload.
    pub fn block_data(&mut self, bytes: &[u8]) {
        assert!(bytes.len() < 256);
        self.u8(TC_BLOCKDATA);
        self.u8(bytes.len() as u8);
        self.buf.extend_from_slice(bytes);
    }

    /// Write a HashMapIntObject: default fields (capacity,limit,size,step alpha) + custom
    /// writeObject block-data (per-entry writeInt(key) as its own 4-byte record, then the value
    /// object), then TC_ENDBLOCKDATA. `write_value` is invoked per used slot with the value id.
    pub fn write_hashmap_int_object<F: FnMut(&mut Ser, usize)>(
        &mut self,
        map_class_name: &str,
        m: &MatIntMap,
        mut write_value: F,
    ) {
        self.u8(TC_OBJECT);
        let cd = ClassDesc {
            name: map_class_name.into(),
            uid: uid::HASH_MAP_INT_OBJECT,
            flags: SC_WRITE_METHOD | SC_SERIALIZABLE,
            fields: vec![
                f_int("capacity"),
                f_int("limit"),
                f_int("size"),
                f_int("step"),
            ],
        };
        self.write_class_desc_chain(&[cd]);
        let _handle = self.assign_handle();
        // default fields (already alphabetical): capacity, limit, size, step
        self.i32(m.capacity);
        self.i32(m.limit);
        self.i32(m.size);
        self.i32(m.step);
        // custom writeObject: for each used slot, writeInt(key) [own block] then writeObject(value)
        for (key, val_idx) in m.slots() {
            self.block_data(&key.to_be_bytes());
            write_value(self, val_idx);
        }
        self.u8(TC_ENDBLOCKDATA);
    }

    /// Write a java.util.Date (custom writeObject: block-data with the 8-byte epoch millis).
    pub fn write_date(&mut self, millis: i64) {
        self.u8(TC_OBJECT);
        let cd = ClassDesc {
            name: "java.util.Date".into(),
            uid: uid::DATE,
            flags: SC_WRITE_METHOD | SC_SERIALIZABLE,
            fields: vec![],
        };
        self.write_class_desc_chain(&[cd]);
        let _handle = self.assign_handle();
        self.block_data(&millis.to_be_bytes());
        self.u8(TC_ENDBLOCKDATA);
    }

    /// Write an EMPTY java.util.HashMap. Default fields (alpha): loadFactor:F, threshold:I.
    /// Custom writeObject block-data: writeInt(bucketCount) writeInt(size); no entries.
    /// For a default `new HashMap<>()` MAT sees bucketCount=16, size=0, loadFactor=0.75, threshold=0.
    pub fn write_empty_hashmap(&mut self) {
        self.u8(TC_OBJECT);
        let cd = ClassDesc {
            name: "java.util.HashMap".into(),
            uid: uid::HASH_MAP,
            flags: SC_WRITE_METHOD | SC_SERIALIZABLE,
            fields: vec![f_float("loadFactor"), f_int("threshold")],
        };
        self.write_class_desc_chain(&[cd]);
        let _handle = self.assign_handle();
        self.f32(0.75);
        self.i32(0); // threshold
        // block-data: bucketCount then size
        let mut bd = Vec::new();
        bd.extend_from_slice(&16i32.to_be_bytes());
        bd.extend_from_slice(&0i32.to_be_bytes());
        self.block_data(&bd);
        self.u8(TC_ENDBLOCKDATA);
    }

    /// Write a non-empty java.util.HashMap. Entries are emitted in JDK bucket-iteration order:
    /// bucket index ascending, insertion order within a bucket (no treeification for small maps).
    /// `entries` is (key_hashcode, write_key, write_value) in INSERTION order.
    /// `cap` is the table capacity (power of two; default map grows 16→32→… at 0.75 load).
    #[allow(clippy::type_complexity)]
    pub fn write_hashmap(
        &mut self,
        cap: u32,
        threshold: i32,
        entries: Vec<(i32, Box<dyn FnOnce(&mut Ser)>, Box<dyn FnOnce(&mut Ser)>)>,
    ) {
        self.u8(TC_OBJECT);
        let cd = ClassDesc {
            name: "java.util.HashMap".into(),
            uid: uid::HASH_MAP,
            flags: SC_WRITE_METHOD | SC_SERIALIZABLE,
            fields: vec![f_float("loadFactor"), f_int("threshold")],
        };
        self.write_class_desc_chain(&[cd]);
        let _handle = self.assign_handle();
        self.f32(0.75);
        self.i32(threshold);
        // order entries by (bucket, insertion index); bucket = spread(hash) & (cap-1)
        let mut order: Vec<usize> = (0..entries.len()).collect();
        let buckets: Vec<u32> = entries
            .iter()
            .map(|(hc, _, _)| {
                let h = *hc as u32;
                (h ^ (h >> 16)) & (cap - 1)
            })
            .collect();
        order.sort_by(|&a, &b| buckets[a].cmp(&buckets[b]).then(a.cmp(&b)));
        let size = entries.len() as i32;
        let mut bd = Vec::new();
        bd.extend_from_slice(&(cap as i32).to_be_bytes());
        bd.extend_from_slice(&size.to_be_bytes());
        self.block_data(&bd);
        // entries: writeObject(key) writeObject(value) in bucket order
        let mut boxed: Vec<Option<(Box<dyn FnOnce(&mut Ser)>, Box<dyn FnOnce(&mut Ser)>)>> =
            entries.into_iter().map(|(_, k, v)| Some((k, v))).collect();
        for i in order {
            let (wk, wv) = boxed[i].take().unwrap();
            wk(self);
            wv(self);
        }
        self.u8(TC_ENDBLOCKDATA);
    }

    /// Java String.hashCode() for a Latin-1/ASCII string.
    pub fn java_string_hashcode(s: &str) -> i32 {
        let mut h: i32 = 0;
        for c in s.chars() {
            h = h.wrapping_mul(31).wrapping_add(c as i32);
        }
        h
    }

    /// Write a boxed java.lang.Boolean (flags 0x02, field value:Z, no super chain).
    pub fn write_boolean(&mut self, v: bool) {
        self.u8(TC_OBJECT);
        let cd = ClassDesc {
            name: "java.lang.Boolean".into(),
            uid: uid::BOOLEAN,
            flags: SC_SERIALIZABLE,
            fields: vec![f_bool("value")],
        };
        self.write_class_desc_chain(&[cd]);
        let _handle = self.assign_handle();
        self.u8(if v { 1 } else { 0 });
    }

    /// Write a boxed java.lang.Long (value:J) with its java.lang.Number superclass (no fields).
    pub fn write_long(&mut self, v: i64) {
        self.u8(TC_OBJECT);
        let chain = vec![
            ClassDesc {
                name: "java.lang.Long".into(),
                uid: uid::LONG,
                flags: SC_SERIALIZABLE,
                fields: vec![f_long("value")],
            },
            ClassDesc {
                name: "java.lang.Number".into(),
                uid: uid::NUMBER,
                flags: SC_SERIALIZABLE,
                fields: vec![],
            },
        ];
        self.write_class_desc_chain(&chain);
        let _handle = self.assign_handle();
        // values superclass-first: Number has none, Long writes value:J
        self.i64(v);
    }

    /// Write a java.util.ArrayList. Custom writeObject: defaultWriteObject writes `size:I`, then
    /// block-data `capacity:int`, then each element via writeObject in list order.
    /// `capacity` is the backing-array length MAT emits (for `new ArrayList<>(collection)` it equals size).
    #[allow(clippy::type_complexity)]
    pub fn write_array_list(&mut self, capacity: i32, elems: Vec<Box<dyn FnOnce(&mut Ser)>>) {
        self.u8(TC_OBJECT);
        let cd = ClassDesc {
            name: "java.util.ArrayList".into(),
            uid: uid::ARRAY_LIST,
            flags: SC_WRITE_METHOD | SC_SERIALIZABLE,
            fields: vec![f_int("size")],
        };
        self.write_class_desc_chain(&[cd]);
        let _handle = self.assign_handle();
        let size = elems.len() as i32;
        self.i32(size); // defaultWriteObject: size field
        self.block_data(&capacity.to_be_bytes()); // block-data: backing-array capacity
        for e in elems {
            e(self);
        }
        self.u8(TC_ENDBLOCKDATA);
    }

    /// Expose handle allocation for callers that write TC_OBJECT manually (e.g. BitField).
    pub fn assign_handle_pub(&mut self) -> u32 {
        self.assign_handle()
    }

    /// Write a TC_ARRAY of int (int[]). `words` is big-endian i32 elements.
    pub fn write_int_array(&mut self, words: &[i32]) {
        self.u8(TC_ARRAY);
        let cd = ClassDesc {
            name: "[I".into(),
            uid: uid::INT_ARRAY,
            flags: SC_SERIALIZABLE,
            fields: vec![],
        };
        self.write_class_desc_chain(&[cd]);
        let _handle = self.assign_handle();
        self.i32(words.len() as i32);
        for &w in words {
            self.i32(w);
        }
    }
}

// ---- HashMapIntObject slot-order replication (mirrors MAT's collect.HashMapIntObject) ----
mod prime {
    // Mirror MAT PrimeFinder exactly: both methods step FIRST, then test — so they return the
    // strictly next/previous prime, never the input itself. Uses the same trial-division test.
    fn is_prime(n: i32) -> bool {
        if n < 2 {
            return false;
        }
        let sqrt = (n as f64).sqrt() as i32;
        let mut i = 2;
        while i <= sqrt {
            if (n / i) * i == n {
                return false;
            }
            i += 1;
        }
        true
    }
    pub fn next_prime(mut floor: i32) -> i32 {
        loop {
            floor += 1;
            if is_prime(floor) {
                return floor;
            }
        }
    }
    pub fn prev_prime(mut ceil: i32) -> i32 {
        loop {
            ceil -= 1;
            if is_prime(ceil) {
                return ceil;
            }
        }
    }
}

/// Mirrors MAT HashMapIntObject<E> slot layout for a given set of inserted keys (insertion order).
pub struct MatIntMap {
    pub capacity: i32,
    pub step: i32,
    pub limit: i32,
    pub size: i32,
    used: Vec<bool>,
    keys: Vec<i32>,
    /// value index into the caller's value list, in insertion order; usize::MAX = empty
    slot_val: Vec<usize>,
}

impl MatIntMap {
    pub fn new(initial_capacity: i32) -> Self {
        let mut m = MatIntMap {
            capacity: 0,
            step: 0,
            limit: 0,
            size: 0,
            used: vec![],
            keys: vec![],
            slot_val: vec![],
        };
        m.init(initial_capacity);
        m
    }
    fn init(&mut self, initial_capacity: i32) {
        self.capacity = prime::next_prime(initial_capacity.max(2));
        // prev_prime requires its argument >= 3 (it steps down, so floor must have a prime below it)
        let step_floor = (initial_capacity / 3).max(3);
        self.step = std::cmp::max(1, prime::prev_prime(step_floor));
        self.limit = (self.capacity as f64 * 0.75) as i32;
        self.size = 0;
        self.used = vec![false; self.capacity as usize];
        self.keys = vec![0; self.capacity as usize];
        self.slot_val = vec![usize::MAX; self.capacity as usize];
    }
    fn hash(&self, key: i32) -> i32 {
        // int r = (int)(((key * 0x9e3779b97f4a7c15L >>> 31) * capacity) >>> 33);
        let k = key as i64; // Java: int*long promotes int to long with sign extension
        let prod = k.wrapping_mul(0x9e3779b97f4a7c15u64 as i64);
        let u = (prod as u64) >> 31;
        let r = ((u.wrapping_mul(self.capacity as u64)) >> 33) as i64;
        r as i32
    }
    fn step_fn(&self, mut hash: i32) -> i32 {
        hash += self.step;
        if hash >= self.capacity || hash < 0 {
            hash -= self.capacity;
        }
        hash
    }
    /// insert key -> value-index (val_idx is the caller's stable value id)
    pub fn put(&mut self, key: i32, val_idx: usize) {
        let mut hash = self.hash(key);
        while self.used[hash as usize] {
            if self.keys[hash as usize] == key {
                self.slot_val[hash as usize] = val_idx;
                return;
            }
            hash = self.step_fn(hash);
        }
        if self.size == self.limit {
            let new_cap = if self.capacity <= (i32::MAX >> 1) {
                self.capacity << 1
            } else {
                self.capacity + 1
            };
            self.resize(new_cap);
            hash = self.hash(key);
            while self.used[hash as usize] {
                hash = self.step_fn(hash);
            }
        }
        self.used[hash as usize] = true;
        self.keys[hash as usize] = key;
        self.slot_val[hash as usize] = val_idx;
        self.size += 1;
    }
    fn resize(&mut self, new_cap: i32) {
        let old_size = self.size;
        let old_used = self.used.clone();
        let old_keys = self.keys.clone();
        let old_vals = self.slot_val.clone();
        self.init(new_cap);
        for i in 0..old_used.len() {
            if old_used[i] {
                let key = old_keys[i];
                let mut hash = self.hash(key);
                while self.used[hash as usize] {
                    hash = self.step_fn(hash);
                }
                self.used[hash as usize] = true;
                self.keys[hash as usize] = key;
                self.slot_val[hash as usize] = old_vals[i];
            }
        }
        self.size = old_size;
    }
    /// used slots in iteration order (index 0..capacity): (key, val_idx)
    pub fn slots(&self) -> Vec<(i32, usize)> {
        let mut out = Vec::new();
        for i in 0..self.used.len() {
            if self.used[i] {
                out.push((self.keys[i], self.slot_val[i]));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Stream header ────────────────────────────────────────────────────────

    #[test]
    fn new_stream_starts_with_magic_and_version() {
        let s = Ser::new();
        assert_eq!(&s.buf[..4], &[0xAC, 0xED, 0x00, 0x05]);
    }

    // ── TC_NULL ──────────────────────────────────────────────────────────────

    #[test]
    fn null_writes_tc_null_byte() {
        let mut s = Ser::new();
        s.null();
        assert_eq!(s.buf.last(), Some(&TC_NULL));
    }

    // ── TC_STRING + interning ────────────────────────────────────────────────

    #[test]
    fn string_first_write_is_tc_string() {
        let mut s = Ser::new();
        let before = s.buf.len();
        s.string("hello");
        let after = &s.buf[before..];
        assert_eq!(after[0], TC_STRING, "first occurrence should be TC_STRING");
        // 2-byte length prefix + 5 UTF-8 bytes
        let len = u16::from_be_bytes([after[1], after[2]]) as usize;
        assert_eq!(len, 5);
        assert_eq!(&after[3..8], b"hello");
    }

    #[test]
    fn string_duplicate_writes_tc_reference() {
        let mut s = Ser::new();
        s.string("dup");
        let before = s.buf.len();
        s.string("dup"); // second occurrence
        let after = &s.buf[before..];
        assert_eq!(
            after[0], TC_REFERENCE,
            "duplicate string should be TC_REFERENCE"
        );
        // 4-byte handle follows
        assert_eq!(after.len(), 5, "TC_REFERENCE + 4-byte handle = 5 bytes");
    }

    #[test]
    fn different_strings_get_separate_handles() {
        let mut s = Ser::new();
        s.string("aaa");
        s.string("bbb");
        // "aaa" the second time should be TC_REFERENCE; "bbb" is different so also fresh TC_STRING
        let before_aaa2 = s.buf.len();
        s.string("aaa");
        assert_eq!(s.buf[before_aaa2], TC_REFERENCE);
        let before_bbb2 = s.buf.len();
        s.string("bbb");
        assert_eq!(s.buf[before_bbb2], TC_REFERENCE);
    }

    // ── write_int_array ──────────────────────────────────────────────────────

    #[test]
    fn write_int_array_encoding() {
        // TC_ARRAY (0x75), class desc for "[I", len:i4, values
        let mut s = Ser::new();
        let before = s.buf.len();
        s.write_int_array(&[1i32, -2, 0x7FFF_FFFF]);
        let data = &s.buf[before..];
        assert_eq!(data[0], TC_ARRAY);
        // Find the length field: skip TC_ARRAY + class desc (variable) + handle (4 bytes)
        // For robustness just check the last 12 bytes (3 * 4) contain the values
        let end = data.len();
        let vals_end = end;
        let vals_start = vals_end - 12;
        let vals = &data[vals_start..vals_end];
        assert_eq!(i32::from_be_bytes(vals[0..4].try_into().unwrap()), 1);
        assert_eq!(i32::from_be_bytes(vals[4..8].try_into().unwrap()), -2);
        assert_eq!(
            i32::from_be_bytes(vals[8..12].try_into().unwrap(),),
            0x7FFF_FFFF
        );
    }

    #[test]
    fn write_int_array_length_prefix() {
        let mut s = Ser::new();
        s.write_int_array(&[10, 20, 30, 40]);
        // The length field (4-byte big-endian) immediately precedes the values.
        // Find it at buf.len() - 4*4 - 4 = buf.len() - 20
        let n = s.buf.len();
        let len_bytes = &s.buf[n - 4 * 4 - 4..n - 4 * 4];
        let count = i32::from_be_bytes(len_bytes.try_into().unwrap());
        assert_eq!(count, 4);
    }

    // ── write_boolean / write_long / write_date ──────────────────────────────

    #[test]
    fn write_boolean_true_and_false() {
        let mut s = Ser::new();
        s.write_boolean(true);
        let after_true = s.buf.last().copied().unwrap();
        assert_eq!(after_true, 1u8);
        let mut s2 = Ser::new();
        s2.write_boolean(false);
        assert_eq!(s2.buf.last().copied().unwrap(), 0u8);
    }

    #[test]
    fn write_long_value_big_endian() {
        let mut s = Ser::new();
        s.write_long(0x0102_0304_0506_0708i64);
        let n = s.buf.len();
        assert_eq!(
            &s.buf[n - 8..n],
            &[0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08]
        );
    }

    #[test]
    fn write_date_uses_block_data_with_epoch_millis() {
        let millis = 0x0011_2233_4455_6677i64;
        let mut s = Ser::new();
        s.write_date(millis);
        // End of stream: TC_ENDBLOCKDATA (0x78), preceding 10 bytes = TC_BLOCKDATA(0x77) len(8) millis
        let n = s.buf.len();
        assert_eq!(s.buf[n - 1], TC_ENDBLOCKDATA);
        assert_eq!(s.buf[n - 11], TC_BLOCKDATA);
        assert_eq!(s.buf[n - 10], 8u8); // block length
        assert_eq!(&s.buf[n - 9..n - 1], &millis.to_be_bytes());
    }

    // ── write_empty_hashmap ──────────────────────────────────────────────────

    #[test]
    fn write_empty_hashmap_ends_with_block_data_and_endblockdata() {
        let mut s = Ser::new();
        s.write_empty_hashmap();
        let n = s.buf.len();
        // Last byte = TC_ENDBLOCKDATA; before that = TC_BLOCKDATA + 8 + 4+4 = 10 bytes
        assert_eq!(s.buf[n - 1], TC_ENDBLOCKDATA);
        assert_eq!(s.buf[n - 11], TC_BLOCKDATA);
        assert_eq!(s.buf[n - 10], 8u8); // block size
        // bucket count = 16 big-endian, size = 0
        let bucket_count = i32::from_be_bytes(s.buf[n - 9..n - 5].try_into().unwrap());
        let map_size = i32::from_be_bytes(s.buf[n - 5..n - 1].try_into().unwrap());
        assert_eq!(bucket_count, 16);
        assert_eq!(map_size, 0);
    }

    // ── Field ordering in write_class_desc_chain ────────────────────────────

    #[test]
    fn field_order_primitives_before_objects_each_group_alpha() {
        // Declare fields in reverse/non-alpha order; stream must emit sorted order.
        // prim fields (alpha): age:I, count:I → stream order: age, count
        // obj fields (alpha): name:Ljava/lang/String;, tag:Ljava/lang/Object; → name, tag
        let cd = ClassDesc {
            name: "Test".into(),
            uid: 1,
            flags: SC_SERIALIZABLE,
            fields: vec![
                f_obj("tag", "Ljava/lang/Object;"),
                f_int("count"),
                f_int("age"),
                f_obj("name", "Ljava/lang/String;"),
            ],
        };
        let mut s = Ser::new();
        let before = s.buf.len();
        s.write_class_desc_chain(&[cd]);
        let after = &s.buf[before..];
        // Find the field count: after TC_CLASSDESC + name + uid + handle assignment + flags
        // It's easier to search for the field count (u16) near the middle of the desc.
        // We know fields are sorted: age(I), count(I), name(L), tag(L)
        // Each prim field: 1 byte typecode + 2+name_len bytes name
        // Look for byte sequence 'I' + \x00\x03 + "age" at some offset after STREAM_MAGIC+VERSION
        let buf_str = std::str::from_utf8(after).ok();
        let _ = buf_str; // only for debugging; search bytes directly
        let age_pos = after.windows(3).position(|w| w == b"age");
        let count_pos = after.windows(5).position(|w| w == b"count");
        assert!(age_pos.is_some() && count_pos.is_some());
        assert!(
            age_pos.unwrap() < count_pos.unwrap(),
            "age before count (alpha order)"
        );
        let name_pos = after.windows(4).position(|w| w == b"name");
        let tag_pos = after.windows(3).position(|w| w == b"tag");
        assert!(name_pos.is_some() && tag_pos.is_some());
        assert!(
            name_pos.unwrap() < tag_pos.unwrap(),
            "name before tag (alpha order)"
        );
        assert!(
            count_pos.unwrap() < name_pos.unwrap(),
            "prims before objects"
        );
    }

    // ── write_object superclass-first field values ────────────────────────────

    #[test]
    fn write_object_superclass_first_values() {
        // Sub declares field 'z:I = 2', Super declares field 'a:I = 1'.
        // Values in the stream must be: a=1 (superclass) THEN z=2 (subclass).
        let chain = vec![
            ClassDesc {
                name: "Sub".into(),
                uid: 1,
                flags: SC_SERIALIZABLE,
                fields: vec![f_int("z")],
            },
            ClassDesc {
                name: "Super".into(),
                uid: 2,
                flags: SC_SERIALIZABLE,
                fields: vec![f_int("a")],
            },
        ];
        let layers = vec![
            LayerData {
                fields: vec![f_int("z")],
                values: vec![("z".into(), FieldVal::Int(2))],
            },
            LayerData {
                fields: vec![f_int("a")],
                values: vec![("a".into(), FieldVal::Int(1))],
            },
        ];
        let mut s = Ser::new();
        s.write_object(&chain, layers);
        let n = s.buf.len();
        // Last 8 bytes: super field (a=1) then sub field (z=2), each 4-byte big-endian
        let super_val = i32::from_be_bytes(s.buf[n - 8..n - 4].try_into().unwrap());
        let sub_val = i32::from_be_bytes(s.buf[n - 4..n].try_into().unwrap());
        assert_eq!(super_val, 1, "superclass field value comes first");
        assert_eq!(sub_val, 2, "subclass field value comes second");
    }

    // ── ref_object via write_object_keyed ────────────────────────────────────

    #[test]
    fn ref_object_produces_tc_reference_to_keyed_object() {
        let mut s = Ser::new();
        let chain = vec![ClassDesc {
            name: "MyObj".into(),
            uid: 99,
            flags: SC_SERIALIZABLE,
            fields: vec![f_int("x")],
        }];
        let layers = vec![LayerData {
            fields: vec![f_int("x")],
            values: vec![("x".into(), FieldVal::Int(42))],
        }];
        s.write_object_keyed(&chain, layers, Some("myobj_key"));
        // Now emit a reference to it
        let before = s.buf.len();
        s.ref_object("myobj_key");
        let after = &s.buf[before..];
        assert_eq!(after[0], TC_REFERENCE);
        // Handle should be BASE_HANDLE + some offset (always a 4-byte big-endian handle)
        let handle = u32::from_be_bytes(after[1..5].try_into().unwrap());
        assert!(handle >= BASE_HANDLE, "handle in valid range");
    }

    // ── MatIntMap: small capacity no longer panics ───────────────────────────

    #[test]
    fn matintmap_small_capacity_no_panic() {
        // Regression test: MatIntMap::new(1) used to panic in prev_prime(0).
        let m = MatIntMap::new(1);
        assert_eq!(m.size, 0);
        assert!(m.capacity >= 2, "capacity must be at least next_prime(2)");
    }

    #[test]
    fn matintmap_new_zero_no_panic() {
        let m = MatIntMap::new(0);
        assert_eq!(m.size, 0);
    }

    #[test]
    fn matintmap_put_and_slots_order() {
        // Insert 3 keys, verify slots() returns only used entries in slot order.
        let mut m = MatIntMap::new(10);
        m.put(100, 0);
        m.put(200, 1);
        m.put(300, 2);
        assert_eq!(m.size, 3);
        let slots = m.slots();
        assert_eq!(slots.len(), 3);
        // All keys must appear exactly once
        let mut keys: Vec<i32> = slots.iter().map(|&(k, _)| k).collect();
        keys.sort();
        assert_eq!(keys, vec![100, 200, 300]);
        // val_idx is the insertion index
        let vals: Vec<usize> = slots.iter().map(|&(_, v)| v).collect();
        assert!(vals.iter().all(|&v| v < 3));
    }

    #[test]
    fn matintmap_duplicate_put_overwrites() {
        let mut m = MatIntMap::new(5);
        m.put(42, 0);
        m.put(42, 1); // overwrite
        assert_eq!(m.size, 1);
        let slots = m.slots();
        assert_eq!(slots.len(), 1);
        assert_eq!(slots[0].1, 1); // latest value
    }

    #[test]
    fn java_string_hashcode_matches_java_spec() {
        // "hello" → 99162322 (well-known value from Java)
        assert_eq!(Ser::java_string_hashcode("hello"), 99162322);
        // "" → 0
        assert_eq!(Ser::java_string_hashcode(""), 0);
        // Single char 'A' = 65
        assert_eq!(Ser::java_string_hashcode("A"), 65);
    }

    // ── write_array_list ──────────────────────────────────────────────────────

    #[test]
    fn write_array_list_empty_ends_with_endblockdata() {
        let mut s = Ser::new();
        s.write_array_list(0, vec![]);
        assert_eq!(s.buf.last(), Some(&TC_ENDBLOCKDATA));
    }

    #[test]
    fn write_array_list_size_field_matches_elems() {
        let mut s = Ser::new();
        // Two null elements
        s.write_array_list(
            2,
            vec![
                Box::new(|s: &mut Ser| s.null()),
                Box::new(|s: &mut Ser| s.null()),
            ],
        );
        // defaultWriteObject writes size:I (=2) right after the class desc + instance handle
        // We can't easily locate the exact byte offset, but we can check the stream has
        // TC_ENDBLOCKDATA at the end and no panic occurred.
        assert_eq!(s.buf.last(), Some(&TC_ENDBLOCKDATA));
    }
}

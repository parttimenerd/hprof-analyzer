//! Read one object's field values straight out of the HPROF dump, by byte
//! offset — no rescan.
//!
//! The analysis pipeline records, per dense object index, the byte offset of
//! that object's record in the (decompressed) HPROF stream
//! (`Pass1::hprof_offsets`, persisted as the cache's `offsets.bin`). With that
//! offset and a *seekable* dump, decoding one object's fields costs a single
//! `seek` plus a few dozen bytes of reads, regardless of how large the dump is.
//!
//! That is the whole point: the OQL `toString(s)` path answers the same question
//! by re-scanning the dump (~50 minutes on 44 GB) and it serializes behind the
//! server's single query slot, so one such query stalls every other client.
//! Reading by offset has neither cost.
//!
//! Scope: `INSTANCE_DUMP`, `OBJ_ARRAY_DUMP` and `PRIM_ARRAY_DUMP` records — a
//! plain Java object, plus the arrays that hold the interesting part of one
//! (`String.value`, `List`/`Map` backing arrays). Class objects have no raw
//! HPROF record and report their kind.

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

use crate::pass1::ClassInfo;
use crate::types::HprofType;

/// HPROF heap-dump sub-record tags.
const INSTANCE_DUMP: u8 = 0x21;
const OBJ_ARRAY_DUMP: u8 = 0x22;
const PRIM_ARRAY_DUMP: u8 = 0x23;

/// How many array elements `read_array_at` decodes before it stops and reports
/// the rest as a count. A `byte[100_000_000]` must not become a 100 MB JSON
/// array — the point is to see what an object *holds*, not to dump it.
pub const MAX_ARRAY_ELEMENTS: usize = 64;

/// Longest string `decode_utf*` returns before truncating (the full length is
/// still reported by the caller).
pub const MAX_STRING_CHARS: usize = 512;

/// One decoded field of an instance.
pub struct InstanceField {
    /// Slash-form name of the class that declares this field. Needed to
    /// disambiguate a subclass field that shadows an inherited one of the same
    /// simple name (e.g. a Scala `PhantomReference.name` over `Thread.name`).
    pub owner: String,
    pub name: String,
    pub ty: HprofType,
    pub value: FieldValue,
}

/// A field's decoded value. Object references are kept as raw addresses —
/// deliberately NOT followed, so the output stays a single object's contents
/// rather than an unbounded tree.
pub enum FieldValue {
    /// Primitive rendered for JSON (numbers, booleans, single-char strings).
    Prim(serde_json::Value),
    /// A non-null object reference: the target's address.
    Ref(u64),
    /// A null object reference.
    Null,
}

/// Whether `path` can be read at an arbitrary byte offset cheaply.
///
/// `Pass1::hprof_offsets` are positions in the *decompressed* stream. For a
/// plain (uncompressed) `.hprof` those are also file offsets, so a `seek` is
/// enough. For gzip / zip / tar.gz inputs they are not, and reaching one would
/// mean decompressing everything before it — exactly the rescan this module
/// exists to avoid — so callers must refuse rather than silently do that.
pub fn is_seekable(path: &Path) -> io::Result<bool> {
    let lower = path.to_string_lossy().to_ascii_lowercase();
    if lower.ends_with(".gz") || lower.ends_with(".tgz") || lower.ends_with(".zip") {
        return Ok(false);
    }
    let mut f = File::open(path)?;
    let mut magic = [0u8; 4];
    let n = f.read(&mut magic)?;
    let gzip = n >= 2 && magic[..2] == [0x1f, 0x8b];
    let zip = n >= 2 && magic[..2] == [0x50, 0x4b];
    Ok(!gzip && !zip)
}

/// Read the `INSTANCE_DUMP` record at `offset` and decode its fields.
///
/// Returns `(object_addr, class_addr, fields)`; `Ok(None)` when `offset` is `0`,
/// which is the sentinel the pipeline writes for class objects (they have no
/// raw HPROF record — see `pass1::scan_heap_segment`).
pub fn read_instance_at(
    path: &Path,
    offset: u64,
    class_map: &HashMap<u64, ClassInfo>,
    strings: &HashMap<u64, String>,
    id_size: usize,
) -> io::Result<Option<(u64, u64, Vec<InstanceField>)>> {
    if offset == 0 {
        return Ok(None);
    }
    let mut f = File::open(path)?;
    f.seek(SeekFrom::Start(offset))?;
    let mut head = [0u8; 1];
    f.read_exact(&mut head)?;
    if head[0] != INSTANCE_DUMP {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "expected INSTANCE_DUMP (0x{INSTANCE_DUMP:02x}) at offset {offset}, \
                 found 0x{:02x} — offsets.bin is stale or the dump changed",
                head[0]
            ),
        ));
    }
    let mut buf = vec![0u8; id_size + 4 + id_size + 4];
    f.read_exact(&mut buf)?;
    let obj_addr = be_u64(&buf[0..id_size]);
    let class_addr = be_u64(&buf[id_size + 4..id_size + 4 + id_size]);
    let data_len = be_u64(&buf[id_size + 4 + id_size..]) as usize;
    if data_len > (1 << 30) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("implausible INSTANCE_DUMP data_len {data_len} at offset {offset}"),
        ));
    }
    let mut blob = vec![0u8; data_len];
    f.read_exact(&mut blob)?;

    let fields = decode_fields(class_addr, &blob, class_map, strings, id_size);
    Ok(Some((obj_addr, class_addr, fields)))
}

/// An array record's decoded contents, capped at the `cap` the caller asked for.
pub struct ArrayRead {
    pub addr: u64,
    /// `Some(ty)` for a primitive array; `None` for an object array, whose
    /// elements are references to instances of `elem_class_addr`.
    pub elem_ty: Option<HprofType>,
    /// Object arrays only: the element class id. `0` for primitive arrays.
    pub elem_class_addr: u64,
    /// Total element count, whether or not every element was decoded.
    pub count: u64,
    /// How many leading elements were skipped (see `read_array_at`'s `skip`).
    pub skipped: u64,
    /// The first `cap` elements at or after `skip` (fewer when the array runs out).
    pub values: Vec<FieldValue>,
}

impl ArrayRead {
    /// True when `values` holds only a slice of the array.
    pub fn truncated(&self) -> bool {
        self.count > self.skipped + self.values.len() as u64
    }
}

/// Read the array record (`OBJ_ARRAY_DUMP` / `PRIM_ARRAY_DUMP`) at `offset`.
///
/// `skip` elements are stepped over before `cap` of them are decoded and
/// returned — a `byte[100_000_000]` must not become a 100 MB JSON array, and a
/// pre-Java-9 `String` whose `value` is a slice of a shared `char[]` must be
/// readable from its own `offset` rather than from element 0. Callers rendering
/// an array pass `skip = 0`.
pub fn read_array_at(
    path: &Path,
    offset: u64,
    id_size: usize,
    skip: u64,
    cap: usize,
) -> io::Result<Option<ArrayRead>> {
    if offset == 0 {
        return Ok(None);
    }
    let mut f = File::open(path)?;
    f.seek(SeekFrom::Start(offset))?;
    let mut tag = [0u8; 1];
    f.read_exact(&mut tag)?;
    let is_obj = match tag[0] {
        OBJ_ARRAY_DUMP => true,
        PRIM_ARRAY_DUMP => false,
        other => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "expected an array record (0x{OBJ_ARRAY_DUMP:02x}/0x{PRIM_ARRAY_DUMP:02x}) \
                     at offset {offset}, found 0x{other:02x} — offsets.bin is stale or the \
                     dump changed"
                ),
            ));
        }
    };
    // addr(id) + stack_serial(u4) + count(u4)
    let mut head = vec![0u8; id_size + 4 + 4];
    f.read_exact(&mut head)?;
    let addr = be_u64(&head[..id_size]);
    let count = be_u64(&head[id_size + 4..id_size + 8]);

    // Object arrays name their element class; primitive arrays name an element type.
    let (elem_ty, elem_class_addr) = if is_obj {
        let mut cbuf = vec![0u8; id_size];
        f.read_exact(&mut cbuf)?;
        (None, be_u64(&cbuf))
    } else {
        let mut code = [0u8; 1];
        f.read_exact(&mut code)?;
        let ty = HprofType::from_code(code[0]).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "unknown primitive element type code {} at offset {offset}",
                    code[0]
                ),
            )
        })?;
        (Some(ty), 0)
    };

    let elem_size = if is_obj {
        id_size
    } else {
        elem_ty.map(|t| t.byte_size()).unwrap_or(0)
    };
    // Never skip past the end: a bogus offset in a corrupt dump would otherwise
    // seek beyond the record and read whatever follows it.
    let skipped = skip.min(count);
    let n = (count - skipped).min(cap as u64) as usize;
    if skipped > 0 && elem_size > 0 {
        let step = (skipped as u128) * (elem_size as u128);
        f.seek(SeekFrom::Current(i64::try_from(step).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("implausible array skip {skipped}×{elem_size} at offset {offset}"),
            )
        })?))?;
    }
    let mut values = Vec::with_capacity(n);
    if elem_size > 0 && n > 0 {
        // n is capped, so this is at most `cap * 8` bytes.
        let mut buf = vec![0u8; n * elem_size];
        f.read_exact(&mut buf)?;
        for chunk in buf.chunks_exact(elem_size) {
            values.push(if let Some(ty) = elem_ty {
                decode_value(ty, chunk, id_size)
            } else {
                let a = be_u64(chunk);
                if a == 0 {
                    FieldValue::Null
                } else {
                    FieldValue::Ref(a)
                }
            });
        }
    }
    Ok(Some(ArrayRead {
        addr,
        elem_ty,
        elem_class_addr,
        count,
        skipped,
        values,
    }))
}

/// Decode a Java 9+ compact `String`'s `byte[] value` with its `byte coder`
/// (`0` = LATIN1, `1` = UTF-16 big-endian) into text.
pub fn text_from_bytes(elems: &[FieldValue], coder: i64) -> String {
    let bytes: Vec<u8> = elems
        .iter()
        .map(|v| match v {
            FieldValue::Prim(serde_json::Value::Number(n)) => n.as_i64().unwrap_or(0) as u8,
            _ => 0,
        })
        .collect();
    if coder == 1 {
        let units: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
            .collect();
        String::from_utf16_lossy(&units)
    } else {
        bytes.iter().map(|&b| b as char).collect()
    }
}

/// Decode a pre-Java-9 `String`'s `char[] value` (each element already rendered
/// as a one-character string by `decode_value`).
pub fn text_from_chars(elems: &[FieldValue]) -> String {
    let mut s = String::new();
    for v in elems {
        if let FieldValue::Prim(serde_json::Value::String(c)) = v {
            s.push_str(c);
        }
    }
    s
}

/// Walk the class's super-chain (child → parent, the order HPROF lays instance
/// field values down in) and decode each field out of `blob`.
///
/// The layout matches `pass2::sizing::field_offset`, which the query engine's
/// `SELECT s.field` uses — the two must agree, so this walks the chain the same
/// way (accumulating widths child-first) rather than re-deriving the rule.
fn decode_fields(
    class_addr: u64,
    blob: &[u8],
    class_map: &HashMap<u64, ClassInfo>,
    strings: &HashMap<u64, String>,
    id_size: usize,
) -> Vec<InstanceField> {
    let mut out = Vec::new();
    let mut off = 0usize;
    let mut cur = class_addr;
    // Guard against a malformed cycle in super_id rather than looping forever.
    let mut hops = 0usize;
    while let Some(ci) = class_map.get(&cur) {
        hops += 1;
        if hops > 10_000 {
            break;
        }
        let owner = strings
            .get(&ci.name_id)
            .cloned()
            .unwrap_or_else(|| format!("<class@0x{cur:x}>"));
        for &(fname_id, ty) in &ci.fields {
            let size = if ty == HprofType::Object {
                id_size
            } else {
                ty.byte_size()
            };
            let name = strings
                .get(&fname_id)
                .cloned()
                .unwrap_or_else(|| format!("<field@{fname_id:x}>"));
            // A truncated blob (shouldn't happen, but a corrupt dump must not
            // panic) yields nulls for the tail fields.
            let value = match blob.get(off..off + size) {
                Some(bytes) => decode_value(ty, bytes, id_size),
                None => FieldValue::Null,
            };
            out.push(InstanceField {
                owner: owner.clone(),
                name,
                ty,
                value,
            });
            off += size;
        }
        if ci.super_id == 0 {
            break;
        }
        cur = ci.super_id;
    }
    out
}

fn decode_value(ty: HprofType, bytes: &[u8], id_size: usize) -> FieldValue {
    match ty {
        HprofType::Object => {
            let addr = be_u64(&bytes[..id_size.min(bytes.len())]);
            if addr == 0 {
                FieldValue::Null
            } else {
                FieldValue::Ref(addr)
            }
        }
        HprofType::Boolean => FieldValue::Prim(serde_json::Value::Bool(bytes[0] != 0)),
        HprofType::Byte => FieldValue::Prim(serde_json::json!(bytes[0] as i8)),
        HprofType::Char => {
            let c = be_u64(bytes) as u32;
            FieldValue::Prim(serde_json::Value::String(
                char::from_u32(c).map(String::from).unwrap_or_default(),
            ))
        }
        HprofType::Short => FieldValue::Prim(serde_json::json!(be_u64(bytes) as u16 as i16)),
        HprofType::Int => FieldValue::Prim(serde_json::json!(be_u64(bytes) as u32 as i32)),
        HprofType::Long => FieldValue::Prim(serde_json::json!(be_u64(bytes) as i64)),
        HprofType::Float => {
            let x = f32::from_bits(be_u64(bytes) as u32) as f64;
            FieldValue::Prim(f64_json(x))
        }
        HprofType::Double => {
            let x = f64::from_bits(be_u64(bytes));
            FieldValue::Prim(f64_json(x))
        }
    }
}

/// `serde_json` has no representation for NaN / ±Inf; render those as strings
/// rather than dropping the field or emitting invalid JSON.
fn f64_json(x: f64) -> serde_json::Value {
    match serde_json::Number::from_f64(x) {
        Some(n) => serde_json::Value::Number(n),
        None => serde_json::Value::String(x.to_string()),
    }
}

/// Big-endian unsigned read of up to 8 bytes (HPROF is a big-endian format).
fn be_u64(bytes: &[u8]) -> u64 {
    let mut v = 0u64;
    for &b in bytes.iter().take(8) {
        v = (v << 8) | b as u64;
    }
    v
}

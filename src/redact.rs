//! `hprof-analyzer redact <INPUT> <OUTPUT>`
//!
//! Two-pass streaming HPROF redactor. Zeroes all primitive field values and
//! array element data while preserving the object graph structure (IDs, class
//! hierarchy, reference fields). A custom top-level record (tag `0xDE`) is
//! prepended so `hprof-analyzer` can detect and label the redacted dump.
//!
//! External tools (MAT, jhat) skip the marker via the standard length-prefixed
//! skip path and open the file normally.

use std::collections::HashMap;
use std::io::{self, Write};

use crate::reader::HprofReader;
use crate::source::HprofSource;
use crate::types::{
    HprofType, heap,
    tags::{self, REDACTED_MARKER},
};

// ── Public entry point ────────────────────────────────────────────────────────

/// Redact `source` and write the result to `writer`.
///
/// `progress(phase, fraction)` is called periodically; `fraction` is in [0,1].
pub fn redact<W: Write>(
    source: &HprofSource,
    mut writer: W,
    progress: impl Fn(&str, f64),
) -> io::Result<()> {
    progress("pass1", 0.0);
    let class_fields = build_class_fields(source)?;
    progress("pass1", 1.0);

    progress("pass2", 0.0);
    let r = source.open()?;
    let id_size = r.id_size;
    let format = r.format.clone();
    let timestamp_ms = r.timestamp_ms;
    write_redacted(
        r,
        &mut writer,
        id_size,
        &format,
        timestamp_ms,
        &class_fields,
        &progress,
    )?;
    progress("pass2", 1.0);

    writer.flush()
}

// ── Pass 1: build class → flattened instance field types ─────────────────────

/// Maps class object-ID → flattened list of instance field types
/// (following super chain) in declaration order (super fields first).
type ClassFields = HashMap<u64, Vec<HprofType>>;

/// Maps class object-ID → super class object-ID (from CLASS_DUMP).
type SuperMap = HashMap<u64, u64>;
/// Maps class object-ID → own declared instance fields (not flattened yet).
type OwnFields = HashMap<u64, Vec<HprofType>>;

fn build_class_fields(source: &HprofSource) -> io::Result<ClassFields> {
    let mut r = source.open()?;
    let id_size = r.id_size;

    let mut own_fields: OwnFields = HashMap::new();
    let mut super_map: SuperMap = HashMap::new();

    loop {
        let (tag, length) = match r.next_record() {
            Ok(None) => break,
            Ok(Some(h)) => h,
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e),
        };
        let result = match tag {
            tags::HEAP_DUMP | tags::HEAP_DUMP_SEGMENT => scan_heap_segment_for_classes(
                &mut r,
                length,
                id_size,
                &mut own_fields,
                &mut super_map,
            ),
            _ => r.skip(length),
        };
        match result {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e),
        }
    }

    // Flatten: for each class, prepend super-chain fields in order.
    let class_ids: Vec<u64> = own_fields.keys().copied().collect();
    let mut flat: ClassFields = HashMap::with_capacity(own_fields.len());
    for id in class_ids {
        let fields = flatten_fields(id, &own_fields, &super_map, 0);
        flat.insert(id, fields);
    }
    Ok(flat)
}

fn flatten_fields(
    id: u64,
    own_fields: &OwnFields,
    super_map: &SuperMap,
    depth: usize,
) -> Vec<HprofType> {
    if depth > 256 {
        return Vec::new(); // guard against cycles in corrupt dumps
    }
    let mut out = Vec::new();
    if let Some(&super_id) = super_map.get(&id) {
        if super_id != 0 {
            out.extend(flatten_fields(super_id, own_fields, super_map, depth + 1));
        }
    }
    if let Some(fields) = own_fields.get(&id) {
        out.extend_from_slice(fields);
    }
    out
}

fn scan_heap_segment_for_classes(
    r: &mut HprofReader,
    length: u64,
    id_size: u8,
    own_fields: &mut OwnFields,
    super_map: &mut SuperMap,
) -> io::Result<()> {
    let ids = id_size as u64;
    let mut remaining = length;
    while remaining > 0 {
        if remaining < 1 {
            break;
        }
        let sub_tag = r.u1()?;
        remaining -= 1;

        let consumed = match sub_tag {
            heap::CLASS_DUMP => scan_class_dump(r, id_size, own_fields, super_map)?,
            heap::INSTANCE_DUMP => {
                // id + u4 stack_serial + class_id + u4 data_len + data
                r.skip(ids + 4 + ids)?;
                let dl = r.u4()? as u64;
                r.skip(dl)?;
                ids + 4 + ids + 4 + dl
            }
            heap::PRIM_ARRAY_DUMP => {
                // id + u4 stack_serial + u4 count + u1 type + elements
                r.skip(ids + 4)?;
                let count = r.u4()? as u64;
                let type_code = r.u1()?;
                let elem_size = elem_byte_size(type_code, id_size);
                r.skip(count * elem_size)?;
                ids + 4 + 4 + 1 + count * elem_size
            }
            heap::OBJ_ARRAY_DUMP => {
                // id + u4 stack_serial + u4 count + class_id + count*id
                r.skip(ids + 4)?;
                let count = r.u4()? as u64;
                r.skip(ids + count * ids)?;
                ids + 4 + 4 + ids + count * ids
            }
            // Root records — sizes vary; use the table from pass1
            heap::ROOT_UNKNOWN => {
                r.skip(ids)?;
                ids
            }
            heap::ROOT_JNI_GLOBAL => {
                r.skip(ids + ids)?;
                ids + ids
            }
            heap::ROOT_JNI_LOCAL | heap::ROOT_JAVA_FRAME => {
                r.skip(ids + 4 + 4)?;
                ids + 8
            }
            heap::ROOT_NATIVE_STACK | heap::ROOT_THREAD_BLOCK => {
                r.skip(ids + 4)?;
                ids + 4
            }
            heap::ROOT_STICKY_CLASS | heap::ROOT_MONITOR_USED => {
                r.skip(ids)?;
                ids
            }
            heap::ROOT_THREAD_OBJ => {
                r.skip(ids + 4 + 4)?;
                ids + 8
            }
            heap::ROOT_INTERNED_STRING
            | heap::ROOT_DEBUGGER
            | heap::ROOT_VM_INTERNAL
            | heap::ROOT_SYSTEM_CLASS => {
                r.skip(ids)?;
                ids
            }
            heap::ROOT_JNI_MONITOR => {
                r.skip(ids + 4 + 4)?;
                ids + 8
            }
            heap::PRIM_ARRAY_NODATA_DUMP => {
                r.skip(ids + 4 + 4 + 1)?;
                ids + 9
            }
            _ => {
                // Unknown sub-tag — can't determine size, stop scanning this segment
                break;
            }
        };
        remaining = remaining.saturating_sub(consumed);
    }
    Ok(())
}

fn scan_class_dump(
    r: &mut HprofReader,
    id_size: u8,
    own_fields: &mut OwnFields,
    super_map: &mut SuperMap,
) -> io::Result<u64> {
    let ids = id_size as u64;
    let mut consumed = 0u64;

    let class_id = r.id()?;
    consumed += ids;
    r.skip(4)?;
    consumed += 4; // stack_trace_serial
    let super_id = r.id()?;
    consumed += ids;
    r.skip(ids * 5)?;
    consumed += ids * 5; // loader + signers + protection_domain + reserved×2
    r.skip(4)?;
    consumed += 4; // instance_size

    // Constant pool
    let cp_count = r.u2()? as u64;
    consumed += 2;
    for _ in 0..cp_count {
        r.skip(2)?;
        consumed += 2; // cp index
        let cp_type = r.u1()?;
        consumed += 1;
        let vs = value_size(cp_type, id_size);
        r.skip(vs)?;
        consumed += vs;
    }

    // Static fields
    let static_count = r.u2()? as u64;
    consumed += 2;
    for _ in 0..static_count {
        r.skip(ids)?;
        consumed += ids; // name_id
        let field_type = r.u1()?;
        consumed += 1;
        let vs = value_size(field_type, id_size);
        r.skip(vs)?;
        consumed += vs;
    }

    // Instance fields
    let field_count = r.u2()? as u64;
    consumed += 2;
    let mut fields = Vec::with_capacity(field_count as usize);
    for _ in 0..field_count {
        r.skip(ids)?;
        consumed += ids; // name_id
        let type_code = r.u1()?;
        consumed += 1;
        let htype = HprofType::from_code(type_code).unwrap_or(HprofType::Int);
        fields.push(htype);
    }

    super_map.insert(class_id, super_id);
    own_fields.insert(class_id, fields);
    Ok(consumed)
}

// ── Pass 2: stream-copy with zeroing ─────────────────────────────────────────

fn write_redacted<W: Write>(
    mut r: HprofReader,
    w: &mut W,
    id_size: u8,
    format: &str,
    timestamp_ms: u64,
    class_fields: &ClassFields,
    progress: &impl Fn(&str, f64),
) -> io::Result<()> {
    // Write HPROF header (format string + NUL + u4 id_size + u8 timestamp).
    w.write_all(format.as_bytes())?;
    w.write_all(&[0u8])?; // NUL terminator
    w.write_all(&(id_size as u32).to_be_bytes())?;
    w.write_all(&timestamp_ms.to_be_bytes())?;

    // Prepend redaction marker record.
    write_redaction_marker(w)?;

    let mut record_count = 0u64;

    loop {
        let (tag, length) = match r.next_record() {
            Ok(None) => break,
            Ok(Some(h)) => h,
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e),
        };
        record_count += 1;
        if record_count % 50_000 == 0 {
            progress("pass2", 0.5); // can't know fraction without file scan; pulse at 0.5
        }

        let result = match tag {
            tags::HEAP_DUMP | tags::HEAP_DUMP_SEGMENT => {
                write_record_header(w, tag, length)?;
                write_redacted_heap_segment(&mut r, w, id_size, length, class_fields)
            }
            // Skip any existing redaction marker — we already wrote one at the
            // top, so re-redacting an already-redacted dump produces exactly one.
            tags::REDACTED_MARKER => r.skip(length),
            _ => {
                write_record_header(w, tag, length)?;
                copy_bytes(&mut r, w, length)
            }
        };
        match result {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e),
        }
    }

    Ok(())
}

fn write_redaction_marker<W: Write>(w: &mut W) -> io::Result<()> {
    // tag (1) + timestamp (4) + length (4) + body (14) = 23 bytes total
    const BODY: &[u8] = b"HPROF-REDACT\x01\x00";
    w.write_all(&[REDACTED_MARKER])?; // tag
    w.write_all(&0u32.to_be_bytes())?; // timestamp
    w.write_all(&(BODY.len() as u32).to_be_bytes())?; // length
    w.write_all(BODY)
}

fn write_record_header<W: Write>(w: &mut W, tag: u8, length: u64) -> io::Result<()> {
    w.write_all(&[tag])?;
    w.write_all(&0u32.to_be_bytes())?; // timestamp (zeroed)
    w.write_all(&(length as u32).to_be_bytes())
}

fn copy_bytes<W: Write>(r: &mut HprofReader, w: &mut W, n: u64) -> io::Result<()> {
    let mut remaining = n as usize;
    let mut buf = vec![0u8; 65536.min(remaining + 1)];
    while remaining > 0 {
        let chunk = remaining.min(buf.len());
        r.read_into(&mut buf[..chunk])?;
        w.write_all(&buf[..chunk])?;
        remaining -= chunk;
    }
    Ok(())
}

fn write_redacted_heap_segment<W: Write>(
    r: &mut HprofReader,
    w: &mut W,
    id_size: u8,
    length: u64,
    class_fields: &ClassFields,
) -> io::Result<()> {
    let ids = id_size as u64;
    let mut remaining = length;

    while remaining > 0 {
        if remaining < 1 {
            break;
        }
        let sub_tag = r.u1()?;
        w.write_all(&[sub_tag])?;
        remaining -= 1;

        let consumed = match sub_tag {
            heap::INSTANCE_DUMP => redact_instance_dump(r, w, id_size, class_fields)?,
            heap::PRIM_ARRAY_DUMP => redact_prim_array_dump(r, w, id_size)?,
            heap::CLASS_DUMP => redact_class_dump(r, w, id_size)?,
            heap::OBJ_ARRAY_DUMP => {
                // Preserved — only object IDs, no user data.
                let count = {
                    copy_id(r, w, ids)?;
                    copy_exact(r, w, 4)?; // stack_serial
                    let count = r.u4()?;
                    w.write_all(&count.to_be_bytes())?;
                    count as u64
                };
                copy_exact(r, w, ids)?; // element class id
                copy_exact(r, w, count * ids)?; // element IDs
                ids + 4 + 4 + ids + count * ids
            }
            // All root records — copy verbatim.
            heap::ROOT_UNKNOWN
            | heap::ROOT_STICKY_CLASS
            | heap::ROOT_MONITOR_USED
            | heap::ROOT_INTERNED_STRING
            | heap::ROOT_DEBUGGER
            | heap::ROOT_VM_INTERNAL
            | heap::ROOT_SYSTEM_CLASS => {
                copy_exact(r, w, ids)?;
                ids
            }
            heap::ROOT_JNI_GLOBAL => {
                copy_exact(r, w, ids + ids)?;
                ids + ids
            }
            heap::ROOT_JNI_LOCAL
            | heap::ROOT_JAVA_FRAME
            | heap::ROOT_THREAD_OBJ
            | heap::ROOT_JNI_MONITOR => {
                copy_exact(r, w, ids + 8)?;
                ids + 8
            }
            heap::ROOT_NATIVE_STACK | heap::ROOT_THREAD_BLOCK => {
                copy_exact(r, w, ids + 4)?;
                ids + 4
            }
            heap::PRIM_ARRAY_NODATA_DUMP => {
                copy_exact(r, w, ids + 9)?;
                ids + 9
            }
            _ => {
                // Unknown sub-tag: can't determine body size — stop.
                break;
            }
        };
        remaining = remaining.saturating_sub(consumed);
    }
    Ok(())
}

/// INSTANCE_DUMP: copy header (id + u4 stack + class_id + u4 data_len),
/// then for each field in flattened type list: copy IDs, write 0x00 for primitives.
fn redact_instance_dump<W: Write>(
    r: &mut HprofReader,
    w: &mut W,
    id_size: u8,
    class_fields: &ClassFields,
) -> io::Result<u64> {
    let ids = id_size as u64;

    let obj_id = r.id()?;
    write_id(w, obj_id, id_size)?;
    copy_exact(r, w, 4)?; // stack_trace_serial
    let class_id = r.id()?;
    write_id(w, class_id, id_size)?;
    let data_len = r.u4()? as u64;
    w.write_all(&(data_len as u32).to_be_bytes())?;

    let header_size = ids + 4 + ids + 4;

    if let Some(fields) = class_fields.get(&class_id) {
        let mut written = 0u64;
        for &ft in fields {
            match ft {
                HprofType::Object => {
                    copy_exact(r, w, ids)?;
                    written += ids;
                }
                prim => {
                    let sz = prim.byte_size() as u64;
                    r.skip(sz)?;
                    w.write_all(&vec![0u8; sz as usize])?;
                    written += sz;
                }
            }
        }
        // If layout doesn't match (corrupt dump or unknown fields), copy remainder verbatim.
        if written < data_len {
            copy_exact(r, w, data_len - written)?;
        } else if written > data_len {
            // Overread — shouldn't happen with valid dumps; skip excess from wire.
        }
    } else {
        // Unknown class — copy body verbatim to stay parseable.
        copy_exact(r, w, data_len)?;
    }

    Ok(header_size + data_len)
}

/// PRIM_ARRAY_DUMP: copy header (id + u4 stack + u4 count + u1 type),
/// write zero bytes for all element data.
fn redact_prim_array_dump<W: Write>(
    r: &mut HprofReader,
    w: &mut W,
    id_size: u8,
) -> io::Result<u64> {
    let ids = id_size as u64;

    copy_exact(r, w, ids + 4)?; // array_id + stack_trace_serial
    let count = r.u4()?;
    w.write_all(&count.to_be_bytes())?;
    let type_code = r.u1()?;
    w.write_all(&[type_code])?;
    let elem_size = elem_byte_size(type_code, id_size);
    let data_len = count as u64 * elem_size;
    r.skip(data_len)?;
    // Write zeroed element data.
    write_zeroes(w, data_len)?;

    Ok(ids + 4 + 4 + 1 + data_len)
}

/// CLASS_DUMP: copy class metadata and field descriptors verbatim;
/// zero static primitive values and constant pool primitive values.
fn redact_class_dump<W: Write>(r: &mut HprofReader, w: &mut W, id_size: u8) -> io::Result<u64> {
    let ids = id_size as u64;
    let mut consumed = 0u64;

    // class_id + stack_serial + super_id + loader_id + signers + domain + reserved×2 + instance_size
    // = ids*7 + 8 bytes
    copy_exact(r, w, ids * 7 + 8)?;
    consumed += ids * 7 + 8;

    // Constant pool — zero primitive values, preserve object refs.
    let cp_count = r.u2()?;
    w.write_all(&cp_count.to_be_bytes())?;
    consumed += 2;
    for _ in 0..cp_count {
        copy_exact(r, w, 2)?; // cp index
        consumed += 2;
        let cp_type = r.u1()?;
        w.write_all(&[cp_type])?;
        consumed += 1;
        let vs = value_size(cp_type, id_size);
        if cp_type == 2 {
            // Object ref — preserve.
            copy_exact(r, w, vs)?;
        } else {
            r.skip(vs)?;
            write_zeroes(w, vs)?;
        }
        consumed += vs;
    }

    // Static fields — zero primitive values, preserve object refs.
    let static_count = r.u2()?;
    w.write_all(&static_count.to_be_bytes())?;
    consumed += 2;
    for _ in 0..static_count {
        copy_exact(r, w, ids)?; // name_id
        consumed += ids;
        let field_type = r.u1()?;
        w.write_all(&[field_type])?;
        consumed += 1;
        let vs = value_size(field_type, id_size);
        if field_type == 2 {
            copy_exact(r, w, vs)?;
        } else {
            r.skip(vs)?;
            write_zeroes(w, vs)?;
        }
        consumed += vs;
    }

    // Instance field descriptors — copy verbatim (just name_id + type_code, no values).
    let field_count = r.u2()?;
    w.write_all(&field_count.to_be_bytes())?;
    consumed += 2;
    let field_desc_size = ids + 1; // name_id + type_code
    copy_exact(r, w, field_count as u64 * field_desc_size)?;
    consumed += field_count as u64 * field_desc_size;

    Ok(consumed)
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn copy_id<W: Write>(r: &mut HprofReader, w: &mut W, ids: u64) -> io::Result<()> {
    copy_exact(r, w, ids)
}

fn copy_exact<W: Write>(r: &mut HprofReader, w: &mut W, n: u64) -> io::Result<()> {
    let mut remaining = n as usize;
    let mut buf = [0u8; 4096];
    while remaining > 0 {
        let chunk = remaining.min(buf.len());
        r.read_into(&mut buf[..chunk])?;
        w.write_all(&buf[..chunk])?;
        remaining -= chunk;
    }
    Ok(())
}

fn write_id<W: Write>(w: &mut W, id: u64, id_size: u8) -> io::Result<()> {
    match id_size {
        4 => w.write_all(&(id as u32).to_be_bytes()),
        _ => w.write_all(&id.to_be_bytes()),
    }
}

fn write_zeroes<W: Write>(w: &mut W, n: u64) -> io::Result<()> {
    const ZEROS: [u8; 4096] = [0u8; 4096];
    let mut remaining = n as usize;
    while remaining > 0 {
        let chunk = remaining.min(ZEROS.len());
        w.write_all(&ZEROS[..chunk])?;
        remaining -= chunk;
    }
    Ok(())
}

fn value_size(type_code: u8, id_size: u8) -> u64 {
    match HprofType::from_code(type_code) {
        Some(HprofType::Object) => id_size as u64,
        Some(t) => t.byte_size() as u64,
        None => 1,
    }
}

fn elem_byte_size(type_code: u8, id_size: u8) -> u64 {
    match HprofType::from_code(type_code) {
        Some(HprofType::Object) => id_size as u64,
        Some(t) => t.byte_size() as u64,
        None => 1,
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::HprofSource;
    use crate::types::{heap, tags};

    // ── HPROF byte builder helpers ────────────────────────────────────────────

    const ID_SIZE: u8 = 4;

    fn header() -> Vec<u8> {
        let mut v = b"JAVA PROFILE 1.0.2\0".to_vec();
        v.extend_from_slice(&(ID_SIZE as u32).to_be_bytes()); // id_size
        v.extend_from_slice(&0u64.to_be_bytes()); // timestamp_ms
        v
    }

    fn u4(n: u32) -> [u8; 4] {
        n.to_be_bytes()
    }
    fn u2(n: u16) -> [u8; 2] {
        n.to_be_bytes()
    }
    fn u1(n: u8) -> [u8; 1] {
        [n]
    }

    // top-level record: tag + timestamp(4) + body_len(4) + body
    fn record(tag: u8, body: &[u8]) -> Vec<u8> {
        let mut v = vec![tag];
        v.extend_from_slice(&0u32.to_be_bytes()); // timestamp
        v.extend_from_slice(&(body.len() as u32).to_be_bytes());
        v.extend_from_slice(body);
        v
    }

    // STRING_IN_UTF8: id(4) + utf8 bytes
    fn string_record(id: u32, s: &str) -> Vec<u8> {
        let mut body = u4(id).to_vec();
        body.extend_from_slice(s.as_bytes());
        record(tags::STRING_IN_UTF8, &body)
    }

    // CLASS_DUMP sub-record (id_size=4, no static/const/instance fields)
    fn class_dump(class_id: u32, super_id: u32, instance_fields: &[(u8, u32)]) -> Vec<u8> {
        let mut v = vec![heap::CLASS_DUMP];
        v.extend_from_slice(&u4(class_id)); // class object id
        v.extend_from_slice(&u4(0)); // stack trace serial
        v.extend_from_slice(&u4(super_id)); // super class id
        v.extend_from_slice(&u4(0)); // loader id
        v.extend_from_slice(&u4(0)); // signers id
        v.extend_from_slice(&u4(0)); // protection domain id
        v.extend_from_slice(&u4(0)); // reserved 1
        v.extend_from_slice(&u4(0)); // reserved 2
        // instance_size — sum of field sizes
        let inst_size: u32 = instance_fields
            .iter()
            .map(|(tc, _)| {
                HprofType::from_code(*tc)
                    .map(|t| {
                        if t == HprofType::Object {
                            ID_SIZE as u32
                        } else {
                            t.byte_size() as u32
                        }
                    })
                    .unwrap_or(1)
            })
            .sum();
        v.extend_from_slice(&u4(inst_size));
        v.extend_from_slice(&u2(0)); // constant pool count = 0
        v.extend_from_slice(&u2(0)); // static fields count = 0
        // instance fields
        v.extend_from_slice(&u2(instance_fields.len() as u16));
        for (type_code, name_id) in instance_fields {
            v.extend_from_slice(&u4(*name_id));
            v.extend_from_slice(&u1(*type_code));
        }
        v
    }

    // INSTANCE_DUMP sub-record (id_size=4)
    fn instance_dump(obj_id: u32, class_id: u32, field_data: &[u8]) -> Vec<u8> {
        let mut v = vec![heap::INSTANCE_DUMP];
        v.extend_from_slice(&u4(obj_id));
        v.extend_from_slice(&u4(0)); // stack serial
        v.extend_from_slice(&u4(class_id));
        v.extend_from_slice(&u4(field_data.len() as u32));
        v.extend_from_slice(field_data);
        v
    }

    // PRIM_ARRAY_DUMP sub-record (id_size=4)
    fn prim_array_dump(arr_id: u32, type_code: u8, elem_data: &[u8]) -> Vec<u8> {
        let elem_size = HprofType::from_code(type_code)
            .map(|t| t.byte_size())
            .unwrap_or(1);
        let count = elem_data.len().checked_div(elem_size).unwrap_or(0);
        let mut v = vec![heap::PRIM_ARRAY_DUMP];
        v.extend_from_slice(&u4(arr_id));
        v.extend_from_slice(&u4(0)); // stack serial
        v.extend_from_slice(&u4(count as u32));
        v.push(type_code);
        v.extend_from_slice(elem_data);
        v
    }

    // OBJ_ARRAY_DUMP sub-record (id_size=4)
    fn obj_array_dump(arr_id: u32, elem_class_id: u32, elem_ids: &[u32]) -> Vec<u8> {
        let mut v = vec![heap::OBJ_ARRAY_DUMP];
        v.extend_from_slice(&u4(arr_id));
        v.extend_from_slice(&u4(0)); // stack serial
        v.extend_from_slice(&u4(elem_ids.len() as u32));
        v.extend_from_slice(&u4(elem_class_id));
        for &id in elem_ids {
            v.extend_from_slice(&u4(id));
        }
        v
    }

    fn heap_dump_record(sub_records: &[u8]) -> Vec<u8> {
        record(tags::HEAP_DUMP, sub_records)
    }

    // Run redact on raw bytes, return redacted bytes.
    fn do_redact(input: &[u8]) -> Vec<u8> {
        let source = HprofSource::from_bytes(input.to_vec(), "test.hprof");
        let mut out = Vec::new();
        redact(&source, &mut out, |_, _| {}).expect("redact failed");
        out
    }

    // Parse redacted output: return (header_bytes, list of (tag, body))
    fn parse_records(data: &[u8]) -> (Vec<u8>, Vec<(u8, Vec<u8>)>) {
        // header = "JAVA PROFILE 1.0.2\0" + u4 id_size + u8 timestamp
        let hdr_len = b"JAVA PROFILE 1.0.2\0".len() + 4 + 8;
        let header = data[..hdr_len].to_vec();
        let mut pos = hdr_len;
        let mut records = Vec::new();
        while pos + 9 <= data.len() {
            let tag = data[pos];
            // skip 4-byte timestamp
            let len = u32::from_be_bytes(data[pos + 5..pos + 9].try_into().unwrap()) as usize;
            pos += 9;
            if pos + len > data.len() {
                break;
            }
            let body = data[pos..pos + len].to_vec();
            pos += len;
            records.push((tag, body));
        }
        (header, records)
    }

    // Count occurrences of tag in record list
    fn count_tag(records: &[(u8, Vec<u8>)], tag: u8) -> usize {
        records.iter().filter(|(t, _)| *t == tag).count()
    }

    // ── Test: marker is present and unique ────────────────────────────────────

    #[test]
    fn marker_present_exactly_once() {
        let mut dump = header();
        dump.extend(string_record(1, "hello"));
        let out = do_redact(&dump);
        let (_, records) = parse_records(&out);
        assert_eq!(
            count_tag(&records, tags::REDACTED_MARKER),
            1,
            "exactly one REDACTED_MARKER record expected"
        );
    }

    #[test]
    fn marker_is_first_record() {
        let mut dump = header();
        dump.extend(string_record(1, "world"));
        let out = do_redact(&dump);
        let (_, records) = parse_records(&out);
        assert_eq!(
            records[0].0,
            tags::REDACTED_MARKER,
            "REDACTED_MARKER must be the first record"
        );
    }

    // ── Test: re-redacting keeps exactly one marker ───────────────────────────

    #[test]
    fn re_redact_produces_exactly_one_marker() {
        let mut dump = header();
        dump.extend(string_record(1, "name"));
        // First redaction
        let first = do_redact(&dump);
        assert_eq!(
            count_tag(&parse_records(&first).1, tags::REDACTED_MARKER),
            1
        );
        // Second redaction of already-redacted output
        let second = do_redact(&first);
        let (_, records) = parse_records(&second);
        assert_eq!(
            count_tag(&records, tags::REDACTED_MARKER),
            1,
            "re-redacting must not accumulate REDACTED_MARKER records"
        );
    }

    #[test]
    fn re_redact_three_times_still_one_marker() {
        let mut dump = header();
        dump.extend(string_record(1, "x"));
        let r1 = do_redact(&dump);
        let r2 = do_redact(&r1);
        let r3 = do_redact(&r2);
        let (_, records) = parse_records(&r3);
        assert_eq!(count_tag(&records, tags::REDACTED_MARKER), 1);
    }

    // ── Test: STRING_IN_UTF8 records are preserved verbatim ───────────────────

    #[test]
    fn strings_are_preserved() {
        let mut dump = header();
        dump.extend(string_record(1, "java/lang/String"));
        dump.extend(string_record(2, "fieldName"));
        let out = do_redact(&dump);
        let (_, records) = parse_records(&out);
        let strs: Vec<&(u8, Vec<u8>)> = records
            .iter()
            .filter(|(t, _)| *t == tags::STRING_IN_UTF8)
            .collect();
        assert_eq!(strs.len(), 2);
        // content includes id(4) + utf8 — check the text is intact
        assert!(strs[0].1.ends_with(b"java/lang/String"));
        assert!(strs[1].1.ends_with(b"fieldName"));
    }

    // ── Test: PRIM_ARRAY_DUMP elements are zeroed ─────────────────────────────

    #[test]
    fn prim_array_byte_elements_zeroed() {
        let elem_data = vec![0xDE, 0xAD, 0xBE, 0xEF, 0x01, 0x02, 0x03, 0x04];
        let sub = prim_array_dump(10, 8 /* Byte */, &elem_data);
        let mut dump = header();
        dump.extend(heap_dump_record(&sub));
        let out = do_redact(&dump);
        // Find prim array in the heap segment body
        let (_, records) = parse_records(&out);
        let heap_body = records.iter().find(|(t, _)| *t == tags::HEAP_DUMP).unwrap();
        // sub-record: tag(1) + id(4) + stack(4) + count(4) + type(1) + data(8)
        let data_offset = 1 + 4 + 4 + 4 + 1;
        let actual_elems = &heap_body.1[data_offset..data_offset + 8];
        assert_eq!(
            actual_elems, &[0u8; 8],
            "byte array elements must be zeroed"
        );
    }

    #[test]
    fn prim_array_int_elements_zeroed() {
        let elem_data: Vec<u8> = vec![0xFF; 12]; // 3 ints, all 0xFF
        let sub = prim_array_dump(11, 10 /* Int */, &elem_data);
        let mut dump = header();
        dump.extend(heap_dump_record(&sub));
        let out = do_redact(&dump);
        let (_, records) = parse_records(&out);
        let heap_body = &records
            .iter()
            .find(|(t, _)| *t == tags::HEAP_DUMP)
            .unwrap()
            .1;
        let data_offset = 1 + 4 + 4 + 4 + 1;
        let actual_elems = &heap_body[data_offset..data_offset + 12];
        assert_eq!(
            actual_elems, &[0u8; 12],
            "int array elements must be zeroed"
        );
    }

    #[test]
    fn prim_array_long_elements_zeroed() {
        let elem_data: Vec<u8> = vec![0xAB; 16]; // 2 longs
        let sub = prim_array_dump(12, 11 /* Long */, &elem_data);
        let mut dump = header();
        dump.extend(heap_dump_record(&sub));
        let out = do_redact(&dump);
        let (_, records) = parse_records(&out);
        let heap_body = &records
            .iter()
            .find(|(t, _)| *t == tags::HEAP_DUMP)
            .unwrap()
            .1;
        let data_offset = 1 + 4 + 4 + 4 + 1;
        let actual_elems = &heap_body[data_offset..data_offset + 16];
        assert_eq!(
            actual_elems, &[0u8; 16],
            "long array elements must be zeroed"
        );
    }

    #[test]
    fn prim_array_char_elements_zeroed() {
        // char array: type code 5, 2 bytes/elem
        let elem_data: Vec<u8> = vec![0x00, 0x48, 0x00, 0x69]; // 'H','i'
        let sub = prim_array_dump(13, 5 /* Char */, &elem_data);
        let mut dump = header();
        dump.extend(heap_dump_record(&sub));
        let out = do_redact(&dump);
        let (_, records) = parse_records(&out);
        let heap_body = &records
            .iter()
            .find(|(t, _)| *t == tags::HEAP_DUMP)
            .unwrap()
            .1;
        let data_offset = 1 + 4 + 4 + 4 + 1;
        let actual_elems = &heap_body[data_offset..data_offset + 4];
        assert_eq!(
            actual_elems, &[0u8; 4],
            "char array elements must be zeroed"
        );
    }

    #[test]
    fn prim_array_header_preserved() {
        // array_id, stack_serial, count, type_code must survive redaction
        let elem_data = vec![0xAA, 0xBB, 0xCC, 0xDD];
        let sub = prim_array_dump(0xCAFE, 8 /* Byte */, &elem_data);
        let mut dump = header();
        dump.extend(heap_dump_record(&sub));
        let out = do_redact(&dump);
        let (_, records) = parse_records(&out);
        let heap_body = &records
            .iter()
            .find(|(t, _)| *t == tags::HEAP_DUMP)
            .unwrap()
            .1;
        // sub-tag
        assert_eq!(heap_body[0], heap::PRIM_ARRAY_DUMP);
        // array_id = 0xCAFE
        let arr_id = u32::from_be_bytes(heap_body[1..5].try_into().unwrap());
        assert_eq!(arr_id, 0xCAFE);
        // count = 4
        let count = u32::from_be_bytes(heap_body[9..13].try_into().unwrap());
        assert_eq!(count, 4);
        // type_code = 8 (Byte)
        assert_eq!(heap_body[13], 8);
    }

    // ── Test: INSTANCE_DUMP primitives zeroed, object refs preserved ──────────

    #[test]
    fn instance_primitive_fields_zeroed() {
        // Class with one int field (type_code=10) and one object ref (type_code=2)
        let class_id = 100u32;
        let super_id = 0u32;
        // fields: int (4 bytes), object ref (4 bytes id_size=4)
        let fields: &[(u8, u32)] = &[(10, 1), (2, 2)]; // int, object
        let class_sub = class_dump(class_id, super_id, fields);

        // Instance: int=0xDEADBEEF, ref=0x00000042
        let mut field_data = vec![];
        field_data.extend_from_slice(&0xDEADBEEFu32.to_be_bytes()); // int value
        field_data.extend_from_slice(&0x00000042u32.to_be_bytes()); // object ref

        let inst_sub = instance_dump(200, class_id, &field_data);
        let mut sub_all = class_sub;
        sub_all.extend(inst_sub);

        let mut dump = header();
        dump.extend(heap_dump_record(&sub_all));
        let out = do_redact(&dump);

        let (_, records) = parse_records(&out);
        let heap_body = &records
            .iter()
            .find(|(t, _)| *t == tags::HEAP_DUMP)
            .unwrap()
            .1;

        // Find INSTANCE_DUMP sub-record: after CLASS_DUMP
        // CLASS_DUMP with 2 instance fields:
        //   tag(1)+class_id(4)+stack(4)+super(4)+loader(4)+signers(4)+domain(4)+res1(4)+res2(4)
        //   +inst_size(4)+cp_count(2)+static_count(2)+inst_field_count(2)+2×(name_id(4)+type(1))
        //   = 33 + 8 + 2 + 10 = 53
        let class_size = 43 + 2 * 5; // base (43) + 2 fields × 5 bytes
        let inst_start = class_size;
        assert_eq!(heap_body[inst_start], heap::INSTANCE_DUMP);

        // instance layout: sub_tag(1) + obj_id(4) + stack(4) + class_id(4) + data_len(4) = 17
        let data_start = inst_start + 17;
        let int_bytes = &heap_body[data_start..data_start + 4];
        let ref_bytes = &heap_body[data_start + 4..data_start + 8];

        assert_eq!(int_bytes, &[0u8; 4], "int field must be zeroed");
        assert_eq!(
            ref_bytes,
            &0x00000042u32.to_be_bytes(),
            "object ref must be preserved"
        );
    }

    #[test]
    fn instance_object_ids_preserved() {
        // Class with two object-ref fields
        let class_id = 101u32;
        let fields: &[(u8, u32)] = &[(2, 1), (2, 2)]; // object, object
        let class_sub = class_dump(class_id, 0, fields);
        let ref1 = 0xAABBCCDDu32;
        let ref2 = 0x11223344u32;
        let mut field_data = vec![];
        field_data.extend_from_slice(&ref1.to_be_bytes());
        field_data.extend_from_slice(&ref2.to_be_bytes());
        let inst_sub = instance_dump(201, class_id, &field_data);
        let mut sub_all = class_sub;
        sub_all.extend(inst_sub);
        let mut dump = header();
        dump.extend(heap_dump_record(&sub_all));
        let out = do_redact(&dump);
        let (_, records) = parse_records(&out);
        let heap_body = &records
            .iter()
            .find(|(t, _)| *t == tags::HEAP_DUMP)
            .unwrap()
            .1;
        let class_size = 43 + 2 * 5; // CLASS_DUMP with 2 instance fields
        let data_start = class_size + 17;
        let got_ref1 =
            u32::from_be_bytes(heap_body[data_start..data_start + 4].try_into().unwrap());
        let got_ref2 = u32::from_be_bytes(
            heap_body[data_start + 4..data_start + 8]
                .try_into()
                .unwrap(),
        );
        assert_eq!(got_ref1, ref1, "first object ref must be preserved");
        assert_eq!(got_ref2, ref2, "second object ref must be preserved");
    }

    #[test]
    fn instance_obj_id_preserved_in_header() {
        let class_id = 102u32;
        let fields: &[(u8, u32)] = &[(10, 1)]; // int
        let class_sub = class_dump(class_id, 0, fields);
        let inst_sub = instance_dump(0xFEEDF00D, class_id, &[0xFF, 0xFF, 0xFF, 0xFF]);
        let mut sub_all = class_sub;
        sub_all.extend(inst_sub);
        let mut dump = header();
        dump.extend(heap_dump_record(&sub_all));
        let out = do_redact(&dump);
        let (_, records) = parse_records(&out);
        let heap_body = &records
            .iter()
            .find(|(t, _)| *t == tags::HEAP_DUMP)
            .unwrap()
            .1;
        let class_size = 48; // CLASS_DUMP with 1 instance field: 43 + 5
        let inst_start = class_size;
        let obj_id = u32::from_be_bytes(
            heap_body[inst_start + 1..inst_start + 5]
                .try_into()
                .unwrap(),
        );
        assert_eq!(obj_id, 0xFEEDF00D, "instance object ID must be preserved");
    }

    // ── Test: OBJ_ARRAY_DUMP element IDs are preserved ────────────────────────

    #[test]
    fn obj_array_element_ids_preserved() {
        let ids = vec![0x01u32, 0x02, 0x03, 0xDEADBEEF];
        let sub = obj_array_dump(300, 50, &ids);
        let mut dump = header();
        dump.extend(heap_dump_record(&sub));
        let out = do_redact(&dump);
        let (_, records) = parse_records(&out);
        let heap_body = &records
            .iter()
            .find(|(t, _)| *t == tags::HEAP_DUMP)
            .unwrap()
            .1;
        // OBJ_ARRAY: tag(1)+id(4)+stack(4)+count(4)+elem_class_id(4)+elems(4*4)
        assert_eq!(heap_body[0], heap::OBJ_ARRAY_DUMP);
        let count = u32::from_be_bytes(heap_body[9..13].try_into().unwrap());
        assert_eq!(count, 4);
        for (i, &expected) in ids.iter().enumerate() {
            let offset = 17 + i * 4;
            let got = u32::from_be_bytes(heap_body[offset..offset + 4].try_into().unwrap());
            assert_eq!(got, expected, "obj array element {i} id must be preserved");
        }
    }

    // ── Test: CLASS_DUMP static fields zeroed, descriptors preserved ──────────

    #[test]
    fn class_dump_static_int_zeroed() {
        // Build CLASS_DUMP with one static int field
        let class_id = 400u32;
        let mut v = vec![heap::CLASS_DUMP];
        v.extend_from_slice(&u4(class_id));
        v.extend_from_slice(&u4(0)); // stack serial
        v.extend_from_slice(&u4(0)); // super_id
        v.extend_from_slice(&u4(0)); // loader
        v.extend_from_slice(&u4(0)); // signers
        v.extend_from_slice(&u4(0)); // domain
        v.extend_from_slice(&u4(0)); // reserved1
        v.extend_from_slice(&u4(0)); // reserved2
        v.extend_from_slice(&u4(0)); // instance_size
        v.extend_from_slice(&u2(0)); // cp_count
        v.extend_from_slice(&u2(1)); // static_count = 1
        v.extend_from_slice(&u4(99)); // static name_id
        v.push(10); // type = Int
        v.extend_from_slice(&0xCAFEBABEu32.to_be_bytes()); // value
        v.extend_from_slice(&u2(0)); // instance field count = 0
        let mut dump = header();
        dump.extend(heap_dump_record(&v));
        let out = do_redact(&dump);
        let (_, records) = parse_records(&out);
        let heap_body = &records
            .iter()
            .find(|(t, _)| *t == tags::HEAP_DUMP)
            .unwrap()
            .1;
        // static value offset: tag(1)+ids(8×4=32)+inst_size(4)+cp_count(2)+static_count(2) = 41,
        // then +name_id(4)+type(1) = 46
        let static_val_offset = 41 + 4 + 1;
        let val = u32::from_be_bytes(
            heap_body[static_val_offset..static_val_offset + 4]
                .try_into()
                .unwrap(),
        );
        assert_eq!(val, 0, "static int field value must be zeroed");
    }

    // ── Test: object graph structure preserved (IDs, count) ──────────────────

    #[test]
    fn object_graph_structure_preserved() {
        // Two classes + two instances + one obj array referencing both
        let c1 = 500u32;
        let c2 = 501u32;
        let i1 = 600u32;
        let i2 = 601u32;
        let a1 = 700u32;
        let fields: &[(u8, u32)] = &[(10, 1)]; // int field
        let mut sub = vec![];
        sub.extend(class_dump(c1, 0, fields));
        sub.extend(class_dump(c2, 0, fields));
        sub.extend(instance_dump(i1, c1, &[0xFFu8; 4]));
        sub.extend(instance_dump(i2, c2, &[0xFFu8; 4]));
        sub.extend(obj_array_dump(a1, c1, &[i1, i2]));
        let mut dump = header();
        dump.extend(heap_dump_record(&sub));
        let out = do_redact(&dump);
        // Parse and verify sub-record count by walking the heap body
        let (_, records) = parse_records(&out);
        let heap_body = &records
            .iter()
            .find(|(t, _)| *t == tags::HEAP_DUMP)
            .unwrap()
            .1;
        let mut pos = 0;
        let mut sub_tags = vec![];
        while pos < heap_body.len() {
            let stag = heap_body[pos];
            sub_tags.push(stag);
            pos += match stag {
                heap::CLASS_DUMP => {
                    let _cp = u16::from_be_bytes(heap_body[pos + 37..pos + 39].try_into().unwrap())
                        as usize;
                    let _sc = u16::from_be_bytes(heap_body[pos + 39..pos + 41].try_into().unwrap())
                        as usize;
                    // skip cp entries (2+1+value_size each), static fields (4+1+value_size each)
                    // For our test all cp_count and static_count are 0, instance fields = 1 * (4+1)
                    let ic = u16::from_be_bytes(heap_body[pos + 41..pos + 43].try_into().unwrap())
                        as usize;
                    43 + ic * 5
                }
                heap::INSTANCE_DUMP => {
                    let dl = u32::from_be_bytes(heap_body[pos + 13..pos + 17].try_into().unwrap())
                        as usize;
                    17 + dl
                }
                heap::OBJ_ARRAY_DUMP => {
                    let cnt = u32::from_be_bytes(heap_body[pos + 9..pos + 13].try_into().unwrap())
                        as usize;
                    17 + cnt * 4
                }
                _ => break,
            };
        }
        assert_eq!(
            sub_tags.iter().filter(|&&t| t == heap::CLASS_DUMP).count(),
            2,
            "2 class dumps"
        );
        assert_eq!(
            sub_tags
                .iter()
                .filter(|&&t| t == heap::INSTANCE_DUMP)
                .count(),
            2,
            "2 instances"
        );
        assert_eq!(
            sub_tags
                .iter()
                .filter(|&&t| t == heap::OBJ_ARRAY_DUMP)
                .count(),
            1,
            "1 obj array"
        );
        // Verify obj array still points to i1, i2
        let arr_pos = sub_tags
            .iter()
            .position(|&t| t == heap::OBJ_ARRAY_DUMP)
            .unwrap();
        let body_offsets: Vec<usize> = {
            let mut offs = vec![];
            let mut p = 0usize;
            for &t in &sub_tags {
                offs.push(p);
                p += match t {
                    heap::CLASS_DUMP => {
                        let ic = u16::from_be_bytes(heap_body[p + 41..p + 43].try_into().unwrap())
                            as usize;
                        43 + ic * 5
                    }
                    heap::INSTANCE_DUMP => {
                        let dl = u32::from_be_bytes(heap_body[p + 13..p + 17].try_into().unwrap())
                            as usize;
                        17 + dl
                    }
                    heap::OBJ_ARRAY_DUMP => {
                        let cnt = u32::from_be_bytes(heap_body[p + 9..p + 13].try_into().unwrap())
                            as usize;
                        17 + cnt * 4
                    }
                    _ => break,
                };
            }
            offs
        };
        let ap = body_offsets[arr_pos];
        let e0 = u32::from_be_bytes(heap_body[ap + 17..ap + 21].try_into().unwrap());
        let e1 = u32::from_be_bytes(heap_body[ap + 21..ap + 25].try_into().unwrap());
        assert_eq!(e0, i1, "obj array[0] id must be preserved");
        assert_eq!(e1, i2, "obj array[1] id must be preserved");
    }

    // ── Test: re-redacting already-redacted dump produces identical output ────

    #[test]
    fn re_redact_is_idempotent() {
        let fields: &[(u8, u32)] = &[(10, 1), (2, 2)]; // int + ref
        let class_sub = class_dump(50, 0, fields);
        let inst_sub = instance_dump(100, 50, &[0xFF, 0xFF, 0xFF, 0xFF, 0x00, 0x00, 0x00, 0x42]);
        let arr_sub = prim_array_dump(200, 8, &[0xAA, 0xBB, 0xCC]);
        let mut sub = class_sub;
        sub.extend(inst_sub);
        sub.extend(arr_sub);
        let mut dump = header();
        dump.extend(string_record(1, "fieldA"));
        dump.extend(heap_dump_record(&sub));

        let r1 = do_redact(&dump);
        let r2 = do_redact(&r1);
        assert_eq!(
            r1, r2,
            "re-redacting an already-redacted dump must produce identical bytes"
        );
    }

    // ── Test: output parses back through pass1 ────────────────────────────────

    #[test]
    fn redacted_output_parseable_by_pass1() {
        let fields: &[(u8, u32)] = &[(10, 1)];
        let class_sub = class_dump(70, 0, fields);
        let inst_sub = instance_dump(150, 70, &[0xFF, 0xFF, 0xFF, 0xFF]);
        let arr_sub = prim_array_dump(250, 8, &[0xAA, 0xBB, 0xCC, 0xDD]);
        let mut sub = class_sub;
        sub.extend(inst_sub);
        sub.extend(arr_sub);
        let mut dump = header();
        dump.extend(heap_dump_record(&sub));
        let redacted = do_redact(&dump);
        let source = HprofSource::from_bytes(redacted, "redacted.hprof");
        let result = crate::pass1::Pass1::run(&source, false);
        assert!(
            result.is_ok(),
            "pass1 must succeed on redacted output: {:?}",
            result.err()
        );
        let p1 = result.unwrap();
        assert!(p1.redacted, "pass1 must detect redacted flag");
    }

    // ── Test: truncated input is handled gracefully ───────────────────────────

    #[test]
    fn truncated_input_does_not_error() {
        let fields: &[(u8, u32)] = &[(10, 1)];
        let class_sub = class_dump(80, 0, fields);
        let inst_sub = instance_dump(160, 80, &[0x01, 0x02, 0x03, 0x04]);
        let arr_sub = prim_array_dump(260, 8, &[0xAA; 100]);
        let mut sub = class_sub;
        sub.extend(inst_sub);
        sub.extend(arr_sub);
        let mut dump = header();
        dump.extend(heap_dump_record(&sub));
        // truncate at various points
        for trunc_at in [
            header().len(),
            header().len() + 5,
            dump.len() / 2,
            dump.len() - 1,
        ] {
            let truncated = dump[..trunc_at].to_vec();
            let source = HprofSource::from_bytes(truncated, "truncated.hprof");
            let result = redact(&source, std::io::sink(), |_, _| {});
            assert!(
                result.is_ok(),
                "truncated input at {trunc_at} must not error"
            );
        }
    }

    // ── Test: empty heap dump (no sub-records) ────────────────────────────────

    #[test]
    fn empty_heap_dump_roundtrip() {
        let mut dump = header();
        dump.extend(heap_dump_record(&[]));
        let out = do_redact(&dump);
        assert!(!out.is_empty());
        let (_, records) = parse_records(&out);
        assert_eq!(count_tag(&records, tags::REDACTED_MARKER), 1);
        assert_eq!(count_tag(&records, tags::HEAP_DUMP), 1);
    }

    // ── Test: header fields preserved ────────────────────────────────────────

    #[test]
    fn header_format_and_id_size_preserved() {
        let dump = header();
        let out = do_redact(&dump);
        assert!(
            out.starts_with(b"JAVA PROFILE 1.0.2\0"),
            "format string preserved"
        );
        let id_size_bytes = &out[b"JAVA PROFILE 1.0.2\0".len()..b"JAVA PROFILE 1.0.2\0".len() + 4];
        let id_size = u32::from_be_bytes(id_size_bytes.try_into().unwrap());
        assert_eq!(id_size, 4, "id_size preserved");
    }

    // ── Test: multiple heap segments ─────────────────────────────────────────

    #[test]
    fn multiple_heap_dump_segments_all_redacted() {
        let fields: &[(u8, u32)] = &[(10, 1)]; // int
        let mut dump = header();
        // First segment: class + instance
        let seg1_class = class_dump(91, 0, fields);
        let seg1_inst = instance_dump(191, 91, &[0xFF; 4]);
        let mut seg1 = seg1_class;
        seg1.extend(seg1_inst);
        dump.extend(record(tags::HEAP_DUMP_SEGMENT, &seg1));
        // Second segment: prim array
        let seg2 = prim_array_dump(291, 10 /* Int */, &[0xFFu8; 8]);
        dump.extend(record(tags::HEAP_DUMP_SEGMENT, &seg2));

        let out = do_redact(&dump);
        let (_, records) = parse_records(&out);

        // Both segments present
        let segs: Vec<_> = records
            .iter()
            .filter(|(t, _)| *t == tags::HEAP_DUMP_SEGMENT)
            .collect();
        assert_eq!(segs.len(), 2, "both HEAP_DUMP_SEGMENT records must survive");

        // Instance in seg1: int field zeroed
        let seg1_body = &segs[0].1;
        let class_size = 48; // CLASS_DUMP with 1 instance field: 43 + 5
        let data_start = class_size + 17;
        assert_eq!(
            &seg1_body[data_start..data_start + 4],
            &[0u8; 4],
            "seg1 int field zeroed"
        );

        // Prim array in seg2: int elements zeroed
        let seg2_body = &segs[1].1;
        let data_offset = 1 + 4 + 4 + 4 + 1;
        assert_eq!(
            &seg2_body[data_offset..data_offset + 8],
            &[0u8; 8],
            "seg2 int array zeroed"
        );
    }

    // ── Test: fixture files ───────────────────────────────────────────────────

    fn fixture_path(name: &str) -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name)
    }

    fn redact_fixture(name: &str) -> Vec<u8> {
        let path = fixture_path(name);
        if !path.exists() {
            return vec![];
        }
        let source = HprofSource::from(path.to_str().unwrap());
        let mut out = Vec::new();
        redact(&source, &mut out, |_, _| {}).expect("redact fixture failed");
        out
    }

    #[test]
    fn fixture_dump1_redact_has_marker() {
        let out = redact_fixture("dump_1_mnemonics.hprof");
        if out.is_empty() {
            return;
        }
        let (_, records) = parse_records(&out);
        assert_eq!(
            count_tag(&records, tags::REDACTED_MARKER),
            1,
            "dump_1 redacted must have exactly one marker"
        );
    }

    #[test]
    fn fixture_dump1_redact_parseable_by_pass1() {
        let out = redact_fixture("dump_1_mnemonics.hprof");
        if out.is_empty() {
            return;
        }
        let source = HprofSource::from_bytes(out, "redacted.hprof");
        let p1 = crate::pass1::Pass1::run(&source, false)
            .expect("pass1 must succeed on redacted fixture");
        assert!(p1.redacted, "pass1 must detect redacted=true on fixture");
    }

    #[test]
    fn fixture_dump1_re_redact_idempotent() {
        let r1 = redact_fixture("dump_1_mnemonics.hprof");
        if r1.is_empty() {
            return;
        }
        let source2 = HprofSource::from_bytes(r1.clone(), "r1.hprof");
        let mut r2 = Vec::new();
        redact(&source2, &mut r2, |_, _| {}).expect("re-redact failed");
        assert_eq!(r1, r2, "re-redacting fixture must be idempotent");
    }

    #[test]
    fn fixture_dump1_re_redact_one_marker() {
        let r1 = redact_fixture("dump_1_mnemonics.hprof");
        if r1.is_empty() {
            return;
        }
        let source2 = HprofSource::from_bytes(r1, "r1.hprof");
        let mut r2 = Vec::new();
        redact(&source2, &mut r2, |_, _| {}).expect("re-redact failed");
        let (_, records) = parse_records(&r2);
        assert_eq!(
            count_tag(&records, tags::REDACTED_MARKER),
            1,
            "re-redacted fixture must have exactly one marker"
        );
    }

    #[test]
    fn fixture_dump1_no_nonzero_prim_arrays() {
        let out = redact_fixture("dump_1_mnemonics.hprof");
        if out.is_empty() {
            return;
        }
        // Extract id_size from header (offset 19, u32 big-endian)
        let hdr_len = b"JAVA PROFILE 1.0.2\0".len();
        let id_size = u32::from_be_bytes(out[hdr_len..hdr_len + 4].try_into().unwrap()) as usize;
        let ids = id_size;

        // Walk the output looking for prim array data — none should be nonzero
        let (_, records) = parse_records(&out);
        for (tag, body) in &records {
            if *tag != tags::HEAP_DUMP && *tag != tags::HEAP_DUMP_SEGMENT {
                continue;
            }
            let mut pos = 0;
            while pos < body.len() {
                if pos >= body.len() {
                    break;
                }
                let stag = body[pos];
                let consumed = match stag {
                    heap::PRIM_ARRAY_DUMP => {
                        // id(ids)+stack(4)+count(4)+type(1)
                        let header_sz = 1 + ids + 4 + 4 + 1;
                        if pos + header_sz > body.len() {
                            break;
                        }
                        let count = u32::from_be_bytes(
                            body[pos + 1 + ids + 4..pos + 1 + ids + 4 + 4]
                                .try_into()
                                .unwrap(),
                        ) as usize;
                        let type_code = body[pos + 1 + ids + 8];
                        let elem_sz = HprofType::from_code(type_code)
                            .map(|t| t.byte_size())
                            .unwrap_or(1);
                        let data_start = pos + header_sz;
                        let data_end = (data_start + count * elem_sz).min(body.len());
                        let all_zero = body[data_start..data_end].iter().all(|&b| b == 0);
                        assert!(
                            all_zero,
                            "prim array at pos {pos} type {type_code} has nonzero data after redaction"
                        );
                        header_sz + count * elem_sz
                    }
                    heap::CLASS_DUMP => {
                        // tag(1)+class_id+stack(4)+super+loader+signers+domain+res1+res2 (ids*7) +inst_size(4)
                        // then cp_count(2)+static_count(2)+inst_field_count(2)
                        let base = 1 + ids * 7 + 8;
                        if pos + base + 6 > body.len() {
                            break;
                        }
                        let mut off = pos + base;
                        let cp =
                            u16::from_be_bytes(body[off..off + 2].try_into().unwrap()) as usize;
                        off += 2;
                        for _ in 0..cp {
                            off += 2 + 1; // cp_index + type
                            if off > body.len() {
                                break;
                            }
                            let ct = body[off - 1];
                            let vs = if ct == 2 {
                                ids
                            } else {
                                HprofType::from_code(ct).map(|t| t.byte_size()).unwrap_or(4)
                            };
                            off += vs;
                        }
                        let sc =
                            u16::from_be_bytes(body[off..off + 2].try_into().unwrap()) as usize;
                        off += 2;
                        for _ in 0..sc {
                            off += ids + 1; // name_id + type
                            if off > body.len() {
                                break;
                            }
                            let ft = body[off - 1];
                            let vs = if ft == 2 {
                                ids
                            } else {
                                HprofType::from_code(ft).map(|t| t.byte_size()).unwrap_or(4)
                            };
                            off += vs;
                        }
                        if off + 2 > body.len() {
                            break;
                        }
                        let ic =
                            u16::from_be_bytes(body[off..off + 2].try_into().unwrap()) as usize;
                        off += 2 + ic * (ids + 1);
                        off - pos
                    }
                    heap::INSTANCE_DUMP => {
                        // tag(1)+id(ids)+stack(4)+class_id(ids)+data_len(4)
                        let hdr = 1 + ids + 4 + ids + 4;
                        if pos + hdr > body.len() {
                            break;
                        }
                        let dl =
                            u32::from_be_bytes(body[pos + hdr - 4..pos + hdr].try_into().unwrap())
                                as usize;
                        hdr + dl
                    }
                    heap::OBJ_ARRAY_DUMP => {
                        // tag(1)+id(ids)+stack(4)+count(4)+class_id(ids)+count×id
                        if pos + 1 + ids + 8 + ids > body.len() {
                            break;
                        }
                        let cnt = u32::from_be_bytes(
                            body[pos + 1 + ids + 4..pos + 1 + ids + 8]
                                .try_into()
                                .unwrap(),
                        ) as usize;
                        1 + ids + 4 + 4 + ids + cnt * ids
                    }
                    heap::ROOT_UNKNOWN
                    | heap::ROOT_STICKY_CLASS
                    | heap::ROOT_MONITOR_USED
                    | heap::ROOT_INTERNED_STRING
                    | heap::ROOT_DEBUGGER
                    | heap::ROOT_VM_INTERNAL
                    | heap::ROOT_SYSTEM_CLASS => 1 + ids,
                    heap::ROOT_JNI_GLOBAL => 1 + ids + ids,
                    heap::ROOT_JNI_LOCAL
                    | heap::ROOT_JAVA_FRAME
                    | heap::ROOT_THREAD_OBJ
                    | heap::ROOT_JNI_MONITOR => 1 + ids + 8,
                    heap::ROOT_NATIVE_STACK | heap::ROOT_THREAD_BLOCK => 1 + ids + 4,
                    heap::PRIM_ARRAY_NODATA_DUMP => 1 + ids + 9,
                    _ => break,
                };
                pos += consumed;
            }
        }
    }

    #[test]
    fn fixture_dump2_redact_parseable() {
        let out = redact_fixture("dump_2_scala-doku.hprof");
        if out.is_empty() {
            return;
        }
        let source = HprofSource::from_bytes(out, "r.hprof");
        let p1 = crate::pass1::Pass1::run(&source, false).expect("pass1 on dump2 redacted");
        assert!(p1.redacted);
    }

    #[test]
    fn fixture_dump4_redact_object_count_matches() {
        let name = "dump_4_philosophers.hprof";
        let path = fixture_path(name);
        if !path.exists() {
            return;
        }
        let orig_source = HprofSource::from(path.to_str().unwrap());
        let orig_p1 = crate::pass1::Pass1::run(&orig_source, false).expect("orig pass1");

        let redacted = redact_fixture(name);
        let red_source = HprofSource::from_bytes(redacted, "r.hprof");
        let red_p1 = crate::pass1::Pass1::run(&red_source, false).expect("redacted pass1");

        assert_eq!(
            orig_p1.instance_count, red_p1.instance_count,
            "instance count must be identical after redaction"
        );
        assert_eq!(
            orig_p1.class_dump_count, red_p1.class_dump_count,
            "class dump count must be identical after redaction"
        );
    }
}

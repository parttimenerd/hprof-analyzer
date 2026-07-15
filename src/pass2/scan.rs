//! Pass-2 low-level heap-record scanners + skip/reader helpers.

use std::io::{self, ErrorKind};

use crate::{
    reader::HprofReader,
    types::{HprofType, heap, tags},
};

/// Full-file sequential scan invoking `f(addr, class_id, &blob)` for each
/// INSTANCE_DUMP whose object address is in `wanted`. Mirrors the heap-record
/// scan skeleton in `scan_heap_2a`/`fill_heap_2b` (streaming-only reader,
/// per-segment sub-record walk). Only the wanted objects' blobs are
/// materialized; everything
/// else is skipped, so RSS stays bounded by `wanted`.
pub(crate) fn scan_instance_blobs<F: FnMut(u64, u64, &[u8])>(
    path: &str,
    id_size: u8,
    wanted: &std::collections::HashSet<u64>,
    mut f: F,
) -> io::Result<()> {
    let ids = id_size as u64;
    let mut r = HprofReader::open(path)?;
    let mut scratch: Vec<u8> = Vec::with_capacity(256);
    loop {
        let tag = match r.u1() {
            Err(e) if e.kind() == ErrorKind::UnexpectedEof => break,
            other => other?,
        };
        let _ts = r.u4()?;
        let length = r.u4()? as u64;
        match tag {
            tags::HEAP_DUMP | tags::HEAP_DUMP_SEGMENT => {
                let mut remaining = length;
                while remaining > 0 {
                    let sub_tag = r.u1()?;
                    remaining -= 1;
                    match sub_tag {
                        heap::ROOT_UNKNOWN | heap::ROOT_MONITOR_USED | heap::ROOT_STICKY_CLASS => {
                            r.skip(ids)?;
                            remaining -= ids;
                        }
                        heap::ROOT_JNI_GLOBAL => {
                            r.skip(2 * ids)?;
                            remaining -= 2 * ids;
                        }
                        heap::ROOT_JNI_LOCAL | heap::ROOT_JAVA_FRAME | heap::ROOT_THREAD_OBJ => {
                            r.skip(ids + 8)?;
                            remaining -= ids + 8;
                        }
                        heap::ROOT_NATIVE_STACK | heap::ROOT_THREAD_BLOCK => {
                            r.skip(ids + 4)?;
                            remaining -= ids + 4;
                        }
                        heap::HEAP_DUMP_INFO => {
                            r.skip(4 + ids)?;
                            remaining -= 4 + ids;
                        }
                        heap::CLASS_DUMP => {
                            let consumed = skip_class_dump(&mut r, id_size)?;
                            remaining -= consumed;
                        }
                        heap::INSTANCE_DUMP => {
                            let addr = r.id()?;
                            r.skip(4)?;
                            let class_id = r.id()?;
                            let data_len = r.u4()? as u64;
                            remaining -= ids + 4 + ids + 4 + data_len;
                            if wanted.contains(&addr) {
                                r.read_bytes_reuse(&mut scratch, data_len as usize)?;
                                f(addr, class_id, &scratch);
                            } else {
                                r.skip(data_len)?;
                            }
                        }
                        heap::OBJ_ARRAY_DUMP => {
                            r.skip(ids + 4)?;
                            let count = r.u4()? as u64;
                            r.skip(ids)?;
                            let byte_len = count.saturating_mul(ids);
                            r.skip(byte_len)?;
                            remaining -= ids + 4 + 4 + ids + byte_len;
                        }
                        heap::PRIM_ARRAY_DUMP => {
                            r.skip(ids + 4)?;
                            let count = r.u4()? as u64;
                            let elem_type = r.u1()?;
                            let esz = HprofType::from_code(elem_type)
                                .map(|t| t.byte_size() as u64)
                                .unwrap_or(1);
                            r.skip(count * esz)?;
                            remaining -= ids + 4 + 4 + 1 + count * esz;
                        }
                        other => {
                            return Err(io::Error::new(
                                ErrorKind::InvalidData,
                                format!("unknown heap sub-tag 0x{other:02x} in thread-name scan"),
                            ));
                        }
                    }
                }
            }
            tags::HEAP_DUMP_END => break,
            _ => r.skip(length)?,
        }
    }
    Ok(())
}

/// Full-file sequential scan invoking `f(addr, elem_bytes)` for each
/// PRIM_ARRAY_DUMP whose array address is in `wanted`. Only wanted arrays'
/// element bytes are materialized; everything else is skipped.
pub(crate) fn scan_prim_arrays<F: FnMut(u64, &[u8])>(
    path: &str,
    id_size: u8,
    wanted: &std::collections::HashSet<u64>,
    mut f: F,
) -> io::Result<()> {
    let ids = id_size as u64;
    let mut r = HprofReader::open(path)?;
    let mut scratch: Vec<u8> = Vec::with_capacity(256);
    loop {
        let tag = match r.u1() {
            Err(e) if e.kind() == ErrorKind::UnexpectedEof => break,
            other => other?,
        };
        let _ts = r.u4()?;
        let length = r.u4()? as u64;
        match tag {
            tags::HEAP_DUMP | tags::HEAP_DUMP_SEGMENT => {
                let mut remaining = length;
                while remaining > 0 {
                    let sub_tag = r.u1()?;
                    remaining -= 1;
                    match sub_tag {
                        heap::ROOT_UNKNOWN | heap::ROOT_MONITOR_USED | heap::ROOT_STICKY_CLASS => {
                            r.skip(ids)?;
                            remaining -= ids;
                        }
                        heap::ROOT_JNI_GLOBAL => {
                            r.skip(2 * ids)?;
                            remaining -= 2 * ids;
                        }
                        heap::ROOT_JNI_LOCAL | heap::ROOT_JAVA_FRAME | heap::ROOT_THREAD_OBJ => {
                            r.skip(ids + 8)?;
                            remaining -= ids + 8;
                        }
                        heap::ROOT_NATIVE_STACK | heap::ROOT_THREAD_BLOCK => {
                            r.skip(ids + 4)?;
                            remaining -= ids + 4;
                        }
                        heap::HEAP_DUMP_INFO => {
                            r.skip(4 + ids)?;
                            remaining -= 4 + ids;
                        }
                        heap::CLASS_DUMP => {
                            let consumed = skip_class_dump(&mut r, id_size)?;
                            remaining -= consumed;
                        }
                        heap::INSTANCE_DUMP => {
                            r.skip(ids + 4)?;
                            let _class_id = r.id()?;
                            let data_len = r.u4()? as u64;
                            r.skip(data_len)?;
                            remaining -= ids + 4 + ids + 4 + data_len;
                        }
                        heap::OBJ_ARRAY_DUMP => {
                            r.skip(ids + 4)?;
                            let count = r.u4()? as u64;
                            r.skip(ids)?;
                            let byte_len = count.saturating_mul(ids);
                            r.skip(byte_len)?;
                            remaining -= ids + 4 + 4 + ids + byte_len;
                        }
                        heap::PRIM_ARRAY_DUMP => {
                            let addr = r.id()?;
                            r.skip(4)?;
                            let count = r.u4()? as u64;
                            let elem_type = r.u1()?;
                            let esz = HprofType::from_code(elem_type)
                                .map(|t| t.byte_size() as u64)
                                .unwrap_or(1);
                            let byte_len = count * esz;
                            remaining -= ids + 4 + 4 + 1 + byte_len;
                            if wanted.contains(&addr) {
                                r.read_bytes_reuse(&mut scratch, byte_len as usize)?;
                                f(addr, &scratch);
                            } else {
                                r.skip(byte_len)?;
                            }
                        }
                        other => {
                            return Err(io::Error::new(
                                ErrorKind::InvalidData,
                                format!("unknown heap sub-tag 0x{other:02x} in thread-name scan"),
                            ));
                        }
                    }
                }
            }
            tags::HEAP_DUMP_END => break,
            _ => r.skip(length)?,
        }
    }
    Ok(())
}

/// Full-file sequential scan invoking `f(addr, elem_ref_bytes)` for each
/// OBJ_ARRAY_DUMP whose array address is in `wanted`. `elem_ref_bytes` is the
/// raw block of `num_elements * id_size` reference bytes (element refs are
/// id_size wide inside an OBJ_ARRAY_DUMP, matching the array-element ref width
/// this scanner skips over). Only wanted arrays are materialized; everything
/// else is skipped, so RSS stays bounded by `wanted`.
pub(crate) fn scan_obj_arrays<F: FnMut(u64, &[u8])>(
    path: &str,
    id_size: u8,
    wanted: &std::collections::HashSet<u64>,
    mut f: F,
) -> io::Result<()> {
    let ids = id_size as u64;
    let mut r = HprofReader::open(path)?;
    let mut scratch: Vec<u8> = Vec::with_capacity(256);
    loop {
        let tag = match r.u1() {
            Err(e) if e.kind() == ErrorKind::UnexpectedEof => break,
            other => other?,
        };
        let _ts = r.u4()?;
        let length = r.u4()? as u64;
        match tag {
            tags::HEAP_DUMP | tags::HEAP_DUMP_SEGMENT => {
                let mut remaining = length;
                while remaining > 0 {
                    let sub_tag = r.u1()?;
                    remaining -= 1;
                    match sub_tag {
                        heap::ROOT_UNKNOWN | heap::ROOT_MONITOR_USED | heap::ROOT_STICKY_CLASS => {
                            r.skip(ids)?;
                            remaining -= ids;
                        }
                        heap::ROOT_JNI_GLOBAL => {
                            r.skip(2 * ids)?;
                            remaining -= 2 * ids;
                        }
                        heap::ROOT_JNI_LOCAL | heap::ROOT_JAVA_FRAME | heap::ROOT_THREAD_OBJ => {
                            r.skip(ids + 8)?;
                            remaining -= ids + 8;
                        }
                        heap::ROOT_NATIVE_STACK | heap::ROOT_THREAD_BLOCK => {
                            r.skip(ids + 4)?;
                            remaining -= ids + 4;
                        }
                        heap::HEAP_DUMP_INFO => {
                            r.skip(4 + ids)?;
                            remaining -= 4 + ids;
                        }
                        heap::CLASS_DUMP => {
                            let consumed = skip_class_dump(&mut r, id_size)?;
                            remaining -= consumed;
                        }
                        heap::INSTANCE_DUMP => {
                            r.skip(ids + 4)?;
                            let _class_id = r.id()?;
                            let data_len = r.u4()? as u64;
                            r.skip(data_len)?;
                            remaining -= ids + 4 + ids + 4 + data_len;
                        }
                        heap::OBJ_ARRAY_DUMP => {
                            let addr = r.id()?;
                            r.skip(4)?; // stack serial
                            let count = r.u4()? as u64;
                            r.skip(ids)?; // array class id
                            let byte_len = count.saturating_mul(ids);
                            remaining -= ids + 4 + 4 + ids + byte_len;
                            if wanted.contains(&addr) {
                                r.read_bytes_reuse(&mut scratch, byte_len as usize)?;
                                f(addr, &scratch);
                            } else {
                                r.skip(byte_len)?;
                            }
                        }
                        heap::PRIM_ARRAY_DUMP => {
                            r.skip(ids + 4)?;
                            let count = r.u4()? as u64;
                            let elem_type = r.u1()?;
                            let esz = HprofType::from_code(elem_type)
                                .map(|t| t.byte_size() as u64)
                                .unwrap_or(1);
                            r.skip(count * esz)?;
                            remaining -= ids + 4 + 4 + 1 + count * esz;
                        }
                        other => {
                            return Err(io::Error::new(
                                ErrorKind::InvalidData,
                                format!("unknown heap sub-tag 0x{other:02x} in obj-array scan"),
                            ));
                        }
                    }
                }
            }
            tags::HEAP_DUMP_END => break,
            _ => r.skip(length)?,
        }
    }
    Ok(())
}

/// Full-file sequential scan invoking `f(class_obj_id, &statics)` for every
/// CLASS_DUMP sub-record, where `statics` is the captured list of static fields
/// as `(name_id, type_code, value)`. Object-typed values are id_size-wide refs;
/// primitive values are zero-extended into the u64. Only the (bounded) static
/// header of each class is materialized — instance-field descriptors are
/// skipped — so RSS stays O(#static-fields-of-one-class) inside the closure.
pub(crate) fn scan_class_dumps<F: FnMut(u64, &[(u64, u8, u64)])>(
    path: &str,
    id_size: u8,
    mut f: F,
) -> io::Result<()> {
    let ids = id_size as u64;
    let mut r = HprofReader::open(path)?;
    let mut statics: Vec<(u64, u8, u64)> = Vec::new();
    let mut vbuf: Vec<u8> = Vec::with_capacity(8);
    loop {
        let tag = match r.u1() {
            Err(e) if e.kind() == ErrorKind::UnexpectedEof => break,
            other => other?,
        };
        let _ts = r.u4()?;
        let length = r.u4()? as u64;
        match tag {
            tags::HEAP_DUMP | tags::HEAP_DUMP_SEGMENT => {
                let mut remaining = length;
                while remaining > 0 {
                    let sub_tag = r.u1()?;
                    remaining -= 1;
                    match sub_tag {
                        heap::ROOT_UNKNOWN | heap::ROOT_MONITOR_USED | heap::ROOT_STICKY_CLASS => {
                            r.skip(ids)?;
                            remaining -= ids;
                        }
                        heap::ROOT_JNI_GLOBAL => {
                            r.skip(2 * ids)?;
                            remaining -= 2 * ids;
                        }
                        heap::ROOT_JNI_LOCAL | heap::ROOT_JAVA_FRAME | heap::ROOT_THREAD_OBJ => {
                            r.skip(ids + 8)?;
                            remaining -= ids + 8;
                        }
                        heap::ROOT_NATIVE_STACK | heap::ROOT_THREAD_BLOCK => {
                            r.skip(ids + 4)?;
                            remaining -= ids + 4;
                        }
                        heap::HEAP_DUMP_INFO => {
                            r.skip(4 + ids)?;
                            remaining -= 4 + ids;
                        }
                        heap::CLASS_DUMP => {
                            let mut consumed = 0u64;
                            let class_obj_id = r.id()?;
                            r.skip(4)?; // stack serial
                            r.skip(ids * 6)?; // super, loader, signer, protdomain, r1, r2
                            r.skip(4)?; // instance size
                            consumed += ids + 4 + ids * 6 + 4;
                            // Constant pool: u2 count, entries (u2 idx, u1 type, value).
                            let cp_count = r.u2()?;
                            consumed += 2;
                            for _ in 0..cp_count {
                                r.skip(2)?;
                                let type_code = r.u1()?;
                                let vs = value_size(type_code, id_size);
                                r.skip(vs)?;
                                consumed += 2 + 1 + vs;
                            }
                            // Static fields: u2 count, entries (name_id, u1 type, value).
                            statics.clear();
                            let static_count = r.u2()?;
                            consumed += 2;
                            for _ in 0..static_count {
                                let name_id = r.id()?;
                                let type_code = r.u1()?;
                                let vs = value_size(type_code, id_size);
                                let value = if vs == 0 {
                                    0
                                } else {
                                    r.read_bytes_reuse(&mut vbuf, vs as usize)?;
                                    // Big-endian value; only OBJECT (id-wide)
                                    // values are consumed downstream, but decode
                                    // any width uniformly into the low bytes.
                                    let mut acc = 0u64;
                                    for &b in vbuf.iter() {
                                        acc = (acc << 8) | b as u64;
                                    }
                                    acc
                                };
                                consumed += ids + 1 + vs;
                                statics.push((name_id, type_code, value));
                            }
                            // Instance fields: u2 count, entries (name_id, u1 type).
                            let inst_count = r.u2()?;
                            consumed += 2;
                            for _ in 0..inst_count {
                                r.skip(ids)?;
                                r.skip(1)?;
                                consumed += ids + 1;
                            }
                            f(class_obj_id, &statics);
                            remaining -= consumed;
                        }
                        heap::INSTANCE_DUMP => {
                            r.skip(ids + 4)?;
                            let _class_id = r.id()?;
                            let data_len = r.u4()? as u64;
                            r.skip(data_len)?;
                            remaining -= ids + 4 + ids + 4 + data_len;
                        }
                        heap::OBJ_ARRAY_DUMP => {
                            r.skip(ids + 4)?;
                            let count = r.u4()? as u64;
                            r.skip(ids)?;
                            let byte_len = count.saturating_mul(ids);
                            r.skip(byte_len)?;
                            remaining -= ids + 4 + 4 + ids + byte_len;
                        }
                        heap::PRIM_ARRAY_DUMP => {
                            r.skip(ids + 4)?;
                            let count = r.u4()? as u64;
                            let elem_type = r.u1()?;
                            let esz = HprofType::from_code(elem_type)
                                .map(|t| t.byte_size() as u64)
                                .unwrap_or(1);
                            r.skip(count * esz)?;
                            remaining -= ids + 4 + 4 + 1 + count * esz;
                        }
                        other => {
                            return Err(io::Error::new(
                                ErrorKind::InvalidData,
                                format!("unknown heap sub-tag 0x{other:02x} in class-dump scan"),
                            ));
                        }
                    }
                }
            }
            tags::HEAP_DUMP_END => break,
            _ => r.skip(length)?,
        }
    }
    Ok(())
}

/// Skip a CLASS_DUMP sub-record, returning the byte count consumed AFTER the
/// 1-byte sub-tag (which the caller has already read). Mirrors the CLASS_DUMP
/// layout in pass1's `read_class_dump`: fixed header, constant pool, static
/// fields, instance-field descriptors.
pub(crate) fn skip_class_dump(r: &mut HprofReader, id_size: u8) -> io::Result<u64> {
    let ids = id_size as u64;
    let mut consumed = 0u64;
    // class_obj_id, stack_serial(4), super_id, loader_id, signer, protdomain,
    // reserved1, reserved2, instance_size(4)
    r.skip(ids)?; // class_obj_id
    r.skip(4)?; // stack serial
    r.skip(ids * 6)?; // super, loader, signer, protection domain, reserved1, reserved2
    r.skip(4)?; // instance_size
    consumed += ids + 4 + ids * 6 + 4;
    // Constant pool: u2 count, then entries of (u2 index, u1 type, value)
    let cp_count = r.u2()?;
    consumed += 2;
    for _ in 0..cp_count {
        r.skip(2)?; // constant pool index
        let type_code = r.u1()?;
        let vs = value_size(type_code, id_size);
        r.skip(vs)?;
        consumed += 2 + 1 + vs;
    }
    // Static fields: u2 count, then (name_id, u1 type, value)
    let static_count = r.u2()?;
    consumed += 2;
    for _ in 0..static_count {
        r.skip(ids)?; // name_id
        let type_code = r.u1()?;
        let vs = value_size(type_code, id_size);
        r.skip(vs)?;
        consumed += ids + 1 + vs;
    }
    // Instance fields: u2 count, then (name_id, u1 type)
    let inst_count = r.u2()?;
    consumed += 2;
    for _ in 0..inst_count {
        r.skip(ids)?; // name_id
        r.skip(1)?; // type
        consumed += ids + 1;
    }
    Ok(consumed)
}

/// Read a big-endian object reference of `ref_size` (4 or 8) bytes from the
/// front of `data`; returns 0 if the slice is too short.
pub(crate) fn read_ref(data: &[u8], ref_size: usize) -> u64 {
    if ref_size == 4 {
        if data.len() >= 4 {
            u32::from_be_bytes([data[0], data[1], data[2], data[3]]) as u64
        } else {
            0
        }
    } else if data.len() >= 8 {
        u64::from_be_bytes([
            data[0], data[1], data[2], data[3], data[4], data[5], data[6], data[7],
        ])
    } else {
        0
    }
}

/// Read a big-endian HPROF id of `id_size` (4 or 8) bytes from the front of
/// `chunk`; returns 0 if the slice is too short.
pub(crate) fn read_id(chunk: &[u8], id_size: u8) -> u64 {
    if id_size == 4 {
        if chunk.len() >= 4 {
            u32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]) as u64
        } else {
            0
        }
    } else if chunk.len() >= 8 {
        u64::from_be_bytes([
            chunk[0], chunk[1], chunk[2], chunk[3], chunk[4], chunk[5], chunk[6], chunk[7],
        ])
    } else {
        0
    }
}

/// Read one id-sized reference directly from the streaming reader.
pub(crate) fn read_id_from_reader(r: &mut HprofReader, _id_size: u8) -> io::Result<u64> {
    r.id()
}

/// On-disk byte width of a static/constant-pool value of the given HPROF type
/// code (Object = `id_size`; unknown code = 0).
pub(crate) fn value_size(type_code: u8, id_size: u8) -> u64 {
    match HprofType::from_code(type_code) {
        Some(HprofType::Object) => id_size as u64,
        Some(t) => t.byte_size() as u64,
        None => 0,
    }
}

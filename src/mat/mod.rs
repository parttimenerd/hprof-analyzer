mod codec;
mod int_index;
mod int_index_1n;
mod serial;

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, BufWriter};
use std::path::{Path, PathBuf};

use crate::cvec::CompressedU32;
use int_index::{IntIndexStreamer, LongIndexStreamer};

#[allow(dead_code)]
pub struct MatEmitter {
    dir: PathBuf,
    prefix: String,
}

#[allow(dead_code)]
impl MatEmitter {
    pub fn new(dir: &Path, prefix: &str) -> io::Result<Self> {
        std::fs::create_dir_all(dir)?;
        Ok(Self {
            dir: dir.to_path_buf(),
            prefix: prefix.to_string(),
        })
    }
    fn path(&self, name: &str) -> PathBuf {
        self.dir.join(format!("{}{}.index", self.prefix, name))
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
        w.into_inner().map_err(|e| e.into_error())?.sync_all()?;
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
        w.into_inner().map_err(|e| e.into_error())?.sync_all()?;
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
        w.into_inner().map_err(|e| e.into_error())?.sync_all()?;
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
        w.into_inner().map_err(|e| e.into_error())?.sync_all()?;
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
        w.into_inner().map_err(|e| e.into_error())?.sync_all()?;
        Ok(())
    }
}

/// MAT `SizeIndexCollectorUncompressed.compress`: maps a byte size (which may
/// exceed i32::MAX for giant arrays) to the int actually stored in `a2s`.
/// Sizes 0..=i32::MAX pass through unchanged; larger values use MAT's lossy
/// divide-by-8 encoding. Negatives clamp to -1.
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
/// Rows with no class object (should not happen for a well-formed heap) keep
/// the sentinel `-1`.
#[allow(dead_code)]
pub fn build_row_to_classobj_id(coc: &HashMap<u32, u32>, num_classes: usize) -> Vec<i32> {
    let mut inv = vec![-1i32; num_classes];
    for (&classobj_id, &row) in coc {
        inv[row as usize] = classobj_id as i32;
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
        let e = MatEmitter::new(&tmp, "dump_").unwrap();
        drop(e);
        assert!(tmp.exists());
    }

    #[test]
    fn inverse_class_table_maps_row_to_classobj_id() {
        // class-object dense id 100 has histogram row 2; id 200 has row 5.
        let mut coc = HashMap::new();
        coc.insert(100u32, 2u32);
        coc.insert(200u32, 5u32);
        let inv = build_row_to_classobj_id(&coc, 6);
        assert_eq!(inv[2], 100);
        assert_eq!(inv[5], 200);
        // untouched rows keep the -1 sentinel
        assert_eq!(inv[0], -1);
        assert_eq!(inv[1], -1);
        assert_eq!(inv[3], -1);
        assert_eq!(inv[4], -1);
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
        let e = MatEmitter::new(&tmp, "dump_").unwrap();
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
        let e = MatEmitter::new(&tmp, "dump_").unwrap();
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
        let e = MatEmitter::new(&tmp, "dump_").unwrap();
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
        let e = MatEmitter::new(&tmp, "dump_").unwrap();
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
        let e = MatEmitter::new(&tmp, "dump_").unwrap();
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
        let e = MatEmitter::new(&tmp, "dump_").unwrap();
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
}

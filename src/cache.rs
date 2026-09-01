//! Disk cache for analysis results — avoids re-running the 5–15 min pipeline.
//!
//! # Layout
//!
//! `<dump_path>.hprof-cache/<hash>/`
//! - `report.json.zst`   — full Report JSON, zstd-3
//! - `pass1.bin.zst`     — Pass1Snapshot (bincode), zstd-3
//! - `arrays.bin`        — 6 dense arrays in indexed format (seekable)
//! - `graph.bin`         — forward/inbound edges (delta-encoded + zstd-3), Graph mode only
//!
//! `hash` = hex(first-64-bytes-of-dump ++ file-size-le8 ++ mtime-ns-le8).
//! Cache busts automatically on any change to the dump file.
//!
//! # File format
//!
//! Both `arrays.bin` and `graph.bin` share a seekable indexed format:
//! ```text
//! [4B magic: b"HPCA"] [2B version: u16 LE = 1] [1B n_arrays]
//! [index: n × { name[16B], offset: u64 LE, len: u64 LE }]
//! [data: concatenated independent zstd frames, one per array]
//! ```

use std::{
    fs::{self, File},
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

use crate::{pass1::Pass1Snapshot, report::Report};

// ── File format constants ─────────────────────────────────────────────────────

const MAGIC: &[u8; 4] = b"HPCA";
const VERSION: u16 = 1;
const ENTRY_SIZE: usize = 16 + 8 + 8; // name[16] + offset:u64 + len:u64

// ── CacheMode ────────────────────────────────────────────────────────────────

/// Which layers to write/check in the cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheMode {
    /// report.json.zst + pass1.bin.zst + arrays.bin (default)
    Full,
    /// Full + graph.bin (forward/inbound CSR; 210–680 MB after delta+zstd-3)
    Graph,
}

// ── Raw data handed to CacheDir for writing ──────────────────────────────────

/// Raw arrays from pass2, slices only (no allocation).
pub struct ArraysForCache<'a> {
    pub shallow: &'a [u32],
    pub class_idx: &'a [u32],
    pub idom: &'a [u32],
    pub retained: &'a [u64],
    pub dc_off: &'a [u32],
    pub dc_tgt: &'a [u32],
    pub n_objects: usize,
}

/// Graph-mode extras (opt-in with --with-graph).
pub struct GraphForCache<'a> {
    pub fwd_off: &'a [u32],
    pub fwd_tgt: &'a [u32],
    pub inb_off: &'a [u32],
    pub inb_tgt: &'a [u32],
    /// Only when --ref-paths was used.
    pub fwd_field_idx: Option<&'a [u16]>,
}

// ── CachedSession ─────────────────────────────────────────────────────────────

/// Everything loaded from a valid cache.
pub struct CachedSession {
    pub dump_path: PathBuf,
    pub report: Report,
    pub pass1: Pass1Snapshot,
    pub cache: CacheDir,
    pub mode: CacheMode,
    /// Dense pass2 class name table: index = class_idx value → class name string.
    pub class_names: Vec<String>,
}

// ── CacheDir ──────────────────────────────────────────────────────────────────

pub struct CacheDir {
    pub path: PathBuf,
}

impl CacheDir {
    /// Compute (or create) the cache directory for the given dump file.
    pub fn for_dump(dump: &Path) -> io::Result<Self> {
        let hash = dump_hash(dump)?;
        let mut dir = dump.to_path_buf();
        // Append ".hprof-cache/<hash>" alongside the dump.
        let name = dir
            .file_name()
            .map(|n| format!("{}.hprof-cache", n.to_string_lossy()))
            .unwrap_or_else(|| "dump.hprof-cache".to_string());
        dir.pop();
        dir.push(name);
        dir.push(&hash);
        fs::create_dir_all(&dir)?;
        Ok(Self { path: dir })
    }

    /// True if the cache is valid for `mode` (checks magic of required files).
    pub fn is_valid(&self, mode: CacheMode) -> bool {
        let arrays_ok = check_magic(&self.path.join("arrays.bin"));
        let pass1_ok = self.path.join("pass1.bin.zst").exists();
        let report_ok = self.path.join("report.json.zst").exists();
        let base_ok = report_ok && pass1_ok && arrays_ok;
        if mode == CacheMode::Graph {
            base_ok && check_magic(&self.path.join("graph.bin"))
        } else {
            base_ok
        }
    }

    // ── Report layer ──────────────────────────────────────────────────────────

    pub fn write_report(&self, r: &Report) -> io::Result<()> {
        let json = serde_json::to_vec(r).map_err(io::Error::other)?;
        write_zstd(&self.path.join("report.json.zst"), &json)
    }

    pub fn read_report(&self) -> io::Result<Option<Report>> {
        let path = self.path.join("report.json.zst");
        if !path.exists() {
            return Ok(None);
        }
        let bytes = read_zstd(&path)?;
        serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(io::Error::other)
    }

    /// Write the dense pass2 class name table (index = class_idx value → name).
    pub fn write_class_names(&self, names: &[String]) -> io::Result<()> {
        let json = serde_json::to_vec(names).map_err(io::Error::other)?;
        write_zstd(&self.path.join("class_names.json.zst"), &json)
    }

    /// Read the class name table written by `write_class_names`.
    pub fn read_class_names(&self) -> io::Result<Option<Vec<String>>> {
        let path = self.path.join("class_names.json.zst");
        if !path.exists() {
            return Ok(None);
        }
        let bytes = read_zstd(&path)?;
        serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(io::Error::other)
    }

    // ── Pass1 layer ───────────────────────────────────────────────────────────

    pub fn write_pass1(&self, p: &Pass1Snapshot) -> io::Result<()> {
        let bin = bincode::serde::encode_to_vec(p, bincode::config::standard())
            .map_err(io::Error::other)?;
        write_zstd(&self.path.join("pass1.bin.zst"), &bin)
    }

    pub fn read_pass1(&self) -> io::Result<Option<Pass1Snapshot>> {
        let path = self.path.join("pass1.bin.zst");
        if !path.exists() {
            return Ok(None);
        }
        let bytes = read_zstd(&path)?;
        let (snap, _) = bincode::serde::decode_from_slice::<Pass1Snapshot, _>(
            &bytes,
            bincode::config::standard(),
        )
        .map_err(io::Error::other)?;
        Ok(Some(snap))
    }

    // ── Array layer ───────────────────────────────────────────────────────────

    pub fn write_arrays(&self, a: &ArraysForCache) -> io::Result<()> {
        let names = [
            "shallow",
            "class_idx",
            "idom",
            "retained",
            "dc_off",
            "dc_tgt",
        ];
        let frames: Vec<Vec<u8>> = vec![
            zstd_compress_u32(a.shallow)?,
            zstd_compress_u32(a.class_idx)?,
            zstd_compress_u32(a.idom)?,
            zstd_compress_u64(a.retained)?,
            zstd_compress_u32(a.dc_off)?,
            zstd_compress_u32(a.dc_tgt)?,
        ];
        write_indexed_file(&self.path.join("arrays.bin"), &names, &frames)
    }

    // ── Graph layer ───────────────────────────────────────────────────────────

    pub fn write_graph(&self, g: &GraphForCache) -> io::Result<()> {
        let mut names: Vec<&str> = vec!["fwd_off", "fwd_tgt", "inb_off", "inb_tgt"];
        let mut frames: Vec<Vec<u8>> = vec![
            zstd_compress_u32(g.fwd_off)?,
            zstd_compress_delta_u32(g.fwd_tgt)?,
            zstd_compress_u32(g.inb_off)?,
            zstd_compress_delta_u32(g.inb_tgt)?,
        ];
        if let Some(fi) = g.fwd_field_idx {
            names.push("fwd_field_idx");
            frames.push(zstd_compress_delta_u16(fi)?);
        }
        write_indexed_file(&self.path.join("graph.bin"), &names, &frames)
    }

    // ── Seek-based reads ──────────────────────────────────────────────────────

    pub fn read_shallow(&self) -> io::Result<Option<Vec<u32>>> {
        read_u32_array(&self.path.join("arrays.bin"), "shallow")
    }

    pub fn read_class_idx(&self) -> io::Result<Option<Vec<u32>>> {
        read_u32_array(&self.path.join("arrays.bin"), "class_idx")
    }

    pub fn read_idom(&self) -> io::Result<Option<Vec<u32>>> {
        read_u32_array(&self.path.join("arrays.bin"), "idom")
    }

    pub fn read_retained(&self) -> io::Result<Option<Vec<u64>>> {
        read_u64_array(&self.path.join("arrays.bin"), "retained")
    }

    /// Returns `(dc_off, dc_tgt)` or `None` if not cached.
    pub fn read_dominator_children(&self) -> io::Result<Option<(Vec<u32>, Vec<u32>)>> {
        let off = read_u32_array(&self.path.join("arrays.bin"), "dc_off")?;
        let tgt = read_u32_array(&self.path.join("arrays.bin"), "dc_tgt")?;
        Ok(match (off, tgt) {
            (Some(o), Some(t)) => Some((o, t)),
            _ => None,
        })
    }

    /// Returns `(fwd_off, fwd_tgt)` or `None` if graph not cached.
    pub fn read_forward_edges(&self) -> io::Result<Option<(Vec<u32>, Vec<u32>)>> {
        let off = read_u32_array(&self.path.join("graph.bin"), "fwd_off")?;
        let tgt = read_delta_u32_array(&self.path.join("graph.bin"), "fwd_tgt")?;
        Ok(match (off, tgt) {
            (Some(o), Some(t)) => Some((o, t)),
            _ => None,
        })
    }

    /// Returns `(inb_off, inb_tgt)` or `None` if graph not cached.
    pub fn read_inbound_csr(&self) -> io::Result<Option<(Vec<u32>, Vec<u32>)>> {
        let off = read_u32_array(&self.path.join("graph.bin"), "inb_off")?;
        let tgt = read_delta_u32_array(&self.path.join("graph.bin"), "inb_tgt")?;
        Ok(match (off, tgt) {
            (Some(o), Some(t)) => Some((o, t)),
            _ => None,
        })
    }

    // ── Cache management ──────────────────────────────────────────────────────

    /// Delete all files in the cache directory.
    pub fn clear(&self) -> io::Result<()> {
        if self.path.exists() {
            fs::remove_dir_all(&self.path)?;
            fs::create_dir_all(&self.path)?;
        }
        Ok(())
    }

    /// Total bytes used by all files in the cache directory.
    pub fn size_bytes(&self) -> u64 {
        dir_size(&self.path)
    }
}

// ── Indexed file format ───────────────────────────────────────────────────────

/// Write the HPCA indexed format: magic + version + index + zstd frames.
fn write_indexed_file(path: &Path, names: &[&str], frames: &[Vec<u8>]) -> io::Result<()> {
    assert_eq!(names.len(), frames.len());
    let n = names.len();
    let index_bytes = n * ENTRY_SIZE;
    let header_bytes = 4 + 2 + 1 + index_bytes; // magic + version + n_arrays + index
    let mut offset = header_bytes as u64;
    let mut entries: Vec<(u64, u64)> = Vec::with_capacity(n);
    for frame in frames {
        let len = frame.len() as u64;
        entries.push((offset, len));
        offset += len;
    }

    let mut f = File::create(path)?;
    f.write_all(MAGIC)?;
    f.write_all(&VERSION.to_le_bytes())?;
    f.write_all(&[n as u8])?;
    for (i, &name) in names.iter().enumerate() {
        let mut name_bytes = [0u8; 16];
        let nb = name.as_bytes();
        name_bytes[..nb.len().min(16)].copy_from_slice(&nb[..nb.len().min(16)]);
        f.write_all(&name_bytes)?;
        f.write_all(&entries[i].0.to_le_bytes())?;
        f.write_all(&entries[i].1.to_le_bytes())?;
    }
    for frame in frames {
        f.write_all(frame)?;
    }
    f.flush()
}

/// Check that a file starts with the HPCA magic bytes.
fn check_magic(path: &Path) -> bool {
    if !path.exists() {
        return false;
    }
    let Ok(mut f) = File::open(path) else {
        return false;
    };
    let mut buf = [0u8; 4];
    f.read_exact(&mut buf).is_ok() && &buf == MAGIC
}

/// Read the array index header from an indexed file.
#[allow(clippy::type_complexity)]
fn read_index(path: &Path) -> io::Result<Option<(File, Vec<([u8; 16], u64, u64)>)>> {
    if !path.exists() {
        return Ok(None);
    }
    let mut f = File::open(path)?;
    let mut magic = [0u8; 4];
    f.read_exact(&mut magic)?;
    if &magic != MAGIC {
        return Ok(None);
    }
    let mut ver = [0u8; 2];
    f.read_exact(&mut ver)?;
    if u16::from_le_bytes(ver) != VERSION {
        return Ok(None);
    }
    let mut n_buf = [0u8; 1];
    f.read_exact(&mut n_buf)?;
    let n = n_buf[0] as usize;
    let mut entries = Vec::with_capacity(n);
    for _ in 0..n {
        let mut name = [0u8; 16];
        let mut off = [0u8; 8];
        let mut len = [0u8; 8];
        f.read_exact(&mut name)?;
        f.read_exact(&mut off)?;
        f.read_exact(&mut len)?;
        entries.push((name, u64::from_le_bytes(off), u64::from_le_bytes(len)));
    }
    Ok(Some((f, entries)))
}

/// Find an entry by name and seek to it, returning (file, compressed_len).
fn seek_to_entry(path: &Path, entry_name: &str) -> io::Result<Option<(File, usize)>> {
    let Some((mut f, entries)) = read_index(path)? else {
        return Ok(None);
    };
    let name_bytes = entry_name.as_bytes();
    for (name, offset, len) in entries {
        if name[..name_bytes.len().min(16)] == name_bytes[..name_bytes.len().min(16)]
            && (name_bytes.len() >= 16 || name[name_bytes.len()] == 0)
        {
            f.seek(SeekFrom::Start(offset))?;
            return Ok(Some((f, len as usize)));
        }
    }
    Ok(None)
}

// ── Array read helpers ────────────────────────────────────────────────────────

fn read_u32_array(path: &Path, name: &str) -> io::Result<Option<Vec<u32>>> {
    let Some((f, len)) = seek_to_entry(path, name)? else {
        return Ok(None);
    };
    let raw = decompress_zstd_frame(f, len)?;
    Ok(Some(le_bytes_to_u32(&raw)))
}

fn read_u64_array(path: &Path, name: &str) -> io::Result<Option<Vec<u64>>> {
    let Some((f, len)) = seek_to_entry(path, name)? else {
        return Ok(None);
    };
    let raw = decompress_zstd_frame(f, len)?;
    Ok(Some(le_bytes_to_u64(&raw)))
}

fn read_delta_u32_array(path: &Path, name: &str) -> io::Result<Option<Vec<u32>>> {
    let Some((f, len)) = seek_to_entry(path, name)? else {
        return Ok(None);
    };
    let raw = decompress_zstd_frame(f, len)?;
    let deltas = le_bytes_to_i32(&raw);
    Ok(Some(undelta_zigzag_i32(&deltas)))
}

// ── Compression helpers ───────────────────────────────────────────────────────

fn write_zstd(path: &Path, data: &[u8]) -> io::Result<()> {
    let enc = zstd::encode_all(data, 3)?;
    fs::write(path, enc)
}

fn read_zstd(path: &Path) -> io::Result<Vec<u8>> {
    let compressed = fs::read(path)?;
    zstd::decode_all(compressed.as_slice())
}

fn zstd_compress_u32(data: &[u32]) -> io::Result<Vec<u8>> {
    let raw = u32_to_le_bytes(data);
    zstd::encode_all(raw.as_slice(), 3)
}

fn zstd_compress_u64(data: &[u64]) -> io::Result<Vec<u8>> {
    let raw = u64_to_le_bytes(data);
    zstd::encode_all(raw.as_slice(), 3)
}

/// Delta-encode a u32 array (zigzag i32 deltas), then zstd-3.
/// Nearly-sorted arrays (like edge targets after dominator ordering) compress 20–50×.
fn zstd_compress_delta_u32(data: &[u32]) -> io::Result<Vec<u8>> {
    let mut deltas: Vec<i32> = Vec::with_capacity(data.len());
    let mut prev = 0i64;
    for &v in data {
        let delta = v as i64 - prev;
        deltas.push(delta as i32);
        prev = v as i64;
    }
    let raw = i32_to_le_bytes(&deltas);
    zstd::encode_all(raw.as_slice(), 3)
}

/// Delta-encode a u16 array, then zstd-3.
fn zstd_compress_delta_u16(data: &[u16]) -> io::Result<Vec<u8>> {
    let mut deltas: Vec<i16> = Vec::with_capacity(data.len());
    let mut prev = 0i32;
    for &v in data {
        let delta = v as i32 - prev;
        deltas.push(delta as i16);
        prev = v as i32;
    }
    let raw = i16_to_le_bytes(&deltas);
    zstd::encode_all(raw.as_slice(), 3)
}

fn undelta_zigzag_i32(deltas: &[i32]) -> Vec<u32> {
    let mut out = Vec::with_capacity(deltas.len());
    let mut acc = 0i64;
    for &d in deltas {
        acc += d as i64;
        out.push(acc as u32);
    }
    out
}

fn decompress_zstd_frame(mut f: File, compressed_len: usize) -> io::Result<Vec<u8>> {
    let mut buf = vec![0u8; compressed_len];
    f.read_exact(&mut buf)?;
    zstd::decode_all(buf.as_slice())
}

// ── Byte-reinterpretation helpers ─────────────────────────────────────────────

fn u32_to_le_bytes(data: &[u32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() * 4);
    for &v in data {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out
}

fn u64_to_le_bytes(data: &[u64]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() * 8);
    for &v in data {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out
}

fn i32_to_le_bytes(data: &[i32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() * 4);
    for &v in data {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out
}

fn i16_to_le_bytes(data: &[i16]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() * 2);
    for &v in data {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out
}

fn le_bytes_to_u32(data: &[u8]) -> Vec<u32> {
    data.chunks_exact(4)
        .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

fn le_bytes_to_u64(data: &[u8]) -> Vec<u64> {
    data.chunks_exact(8)
        .map(|c| u64::from_le_bytes([c[0], c[1], c[2], c[3], c[4], c[5], c[6], c[7]]))
        .collect()
}

fn le_bytes_to_i32(data: &[u8]) -> Vec<i32> {
    data.chunks_exact(4)
        .map(|c| i32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

// ── Hash / filesystem helpers ──────────────────────────────────────────────────

/// Hash a dump file for cache-busting: hex(first-64-bytes ++ file-size-le8 ++ mtime-ns-le8).
fn dump_hash(path: &Path) -> io::Result<String> {
    let meta = fs::metadata(path)?;
    let file_size = meta.len();
    let mtime_ns = meta
        .modified()
        .ok()
        .and_then(|t| {
            t.duration_since(std::time::UNIX_EPOCH)
                .ok()
                .map(|d| d.as_nanos() as u64)
        })
        .unwrap_or(0);

    let mut f = File::open(path)?;
    let mut header = [0u8; 64];
    let n = f.read(&mut header)?;
    let header = &header[..n];

    // Simple but collision-resistant hash: FNV-1a over all input bytes.
    let mut h: u64 = 14695981039346656037u64;
    for &b in header {
        h ^= b as u64;
        h = h.wrapping_mul(1099511628211);
    }
    for &b in &file_size.to_le_bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(1099511628211);
    }
    for &b in &mtime_ns.to_le_bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(1099511628211);
    }
    Ok(format!("{h:016x}"))
}

fn dir_size(path: &Path) -> u64 {
    let Ok(rd) = fs::read_dir(path) else {
        return 0;
    };
    rd.filter_map(|e| e.ok())
        .filter_map(|e| e.metadata().ok())
        .filter(|m| m.is_file())
        .map(|m| m.len())
        .sum()
}

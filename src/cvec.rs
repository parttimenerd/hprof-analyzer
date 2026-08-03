//! Compressed holders for the large "cold" per-object arrays (shallow,
//! class_idx) that sit idle in RAM across the rpo -> inbound -> dominator peak
//! window. Compress right after they are built, hold the small blob across the
//! peak, and restore the full `Vec<u32>` only when a consumer needs random
//! access. deflate9 (flate2) is used; it is pure-Rust and WASM-compatible.

use std::io::{self, Read, Write};

/// Which codec to use across the peak window.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Codec {
    /// No compression: keep the live Vec (no RSS win; A/B escape hatch).
    None,
    /// deflate at max level (flate2 Compression::best()).
    Deflate9,
}

impl Codec {
    /// Parse a codec name; test-only A/B helper.
    #[cfg(test)]
    pub fn parse(s: &str) -> Option<Codec> {
        match s {
            "none" => Some(Codec::None),
            "deflate9" | "deflate" => Some(Codec::Deflate9),
            _ => None,
        }
    }
}

fn deflate_compress(raw: &[u8]) -> io::Result<Vec<u8>> {
    let mut e = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::best());
    e.write_all(raw)?;
    e.finish()
}

fn deflate_decompress(blob: &[u8], cap: usize) -> io::Result<Vec<u8>> {
    let mut d = flate2::read::DeflateDecoder::new(blob);
    let mut out = Vec::with_capacity(cap);
    d.read_to_end(&mut out)?;
    Ok(out)
}

/// A `Vec<u32>` held compressed across the peak window, restorable losslessly.
///
/// With `Codec::None` this keeps the live `Vec<u32>` unchanged (no free); with
/// `Codec::Deflate9` it holds only a compressed blob of the LE bytes and the
/// original element count.
pub struct CompressedU32 {
    codec: Codec,
    /// Compressed blob (Deflate9) or empty (None).
    blob: Vec<u8>,
    /// Live copy for the None codec (empty for Deflate9).
    raw: Vec<u32>,
    len: usize,
}

impl CompressedU32 {
    /// Compress `v` under `codec`.
    pub fn compress(v: &[u32], codec: Codec) -> io::Result<Self> {
        let len = v.len();
        match codec {
            Codec::None => Ok(Self {
                codec,
                blob: Vec::new(),
                raw: v.to_vec(),
                len,
            }),
            Codec::Deflate9 => {
                let mut bytes = Vec::with_capacity(len * 4);
                for &x in v {
                    bytes.extend_from_slice(&x.to_le_bytes());
                }
                let blob = deflate_compress(&bytes)?;
                Ok(Self {
                    codec,
                    blob,
                    raw: Vec::new(),
                    len,
                })
            }
        }
    }

    /// Restore the full `Vec<u32>` (byte-identical to the original input).
    /// Uses a streaming 64 KiB decoder to avoid materializing a full-size byte
    /// intermediate (which would transiently double peak RSS to ~4 GB on large dumps).
    pub fn restore(&self) -> io::Result<Vec<u32>> {
        match self.codec {
            Codec::None => Ok(self.raw.clone()),
            Codec::Deflate9 => {
                let mut out = Vec::with_capacity(self.len);
                self.for_each_u32(|x| out.push(x))?;
                Ok(out)
            }
        }
    }

    /// Stream the decompressed `u32` sequence through `f` WITHOUT ever holding
    /// the full decompressed buffer. Keeps the transient O(64 KiB) rather than
    /// O(n). For `Codec::None` the live `Vec<u32>` is iterated directly.
    pub fn for_each_u32<F: FnMut(u32)>(&self, mut f: F) -> io::Result<()> {
        match self.codec {
            Codec::None => {
                for &x in &self.raw {
                    f(x);
                }
                Ok(())
            }
            Codec::Deflate9 => {
                stream_u32s(flate2::read::DeflateDecoder::new(&self.blob[..]), &mut f)
            }
        }
    }

    /// Decompress the element at `target_idx` by streaming through the data. O(n).
    /// Use only for single-element lookups; prefer `restore()` for batch access.
    pub fn get_at(&self, target_idx: usize) -> io::Result<Option<u32>> {
        match self.codec {
            Codec::None => Ok(self.raw.get(target_idx).copied()),
            Codec::Deflate9 => {
                let mut cur = 0usize;
                let mut found = None;
                self.for_each_u32(|x| {
                    if cur == target_idx {
                        found = Some(x);
                    }
                    cur += 1;
                })?;
                Ok(found)
            }
        }
    }

    /// Decompress elements in `[start, end)` by streaming. O(end).
    /// Use only for range lookups; prefer `restore()` for batch access.
    pub fn slice_at(&self, start: usize, end: usize) -> io::Result<Vec<u32>> {
        if start >= end {
            return Ok(Vec::new());
        }
        match self.codec {
            Codec::None => {
                let len = self.raw.len();
                Ok(self.raw[start.min(len)..end.min(len)].to_vec())
            }
            Codec::Deflate9 => {
                let mut result = Vec::with_capacity(end - start);
                let mut i = 0usize;
                self.for_each_u32(|x| {
                    if i >= start && i < end {
                        result.push(x);
                    }
                    i += 1;
                })?;
                Ok(result)
            }
        }
    }

    /// Bytes currently held (blob for Deflate9, raw*4 for None).
    #[allow(dead_code)]
    pub fn held_bytes(&self) -> usize {
        match self.codec {
            Codec::None => self.raw.len() * 4,
            Codec::Deflate9 => self.blob.len(),
        }
    }
}

/// Decode a stream of LE u32s from `r`, calling `f` for each one.
/// Uses a fixed 64 KiB buffer so the transient is O(64 KiB).
fn stream_u32s<R: Read, F: FnMut(u32)>(mut r: R, f: &mut F) -> io::Result<()> {
    let mut buf = [0u8; 64 * 1024];
    let mut carry: [u8; 4] = [0; 4];
    let mut carry_len = 0usize;
    loop {
        let n = r.read(&mut buf)?;
        if n == 0 {
            break;
        }
        let mut i = 0usize;
        // Complete a partial u32 left over from the previous read.
        while carry_len > 0 && i < n {
            carry[carry_len] = buf[i];
            carry_len += 1;
            i += 1;
            if carry_len == 4 {
                f(u32::from_le_bytes(carry));
                carry_len = 0;
            }
        }
        // Whole u32s inside this buffer.
        while i + 4 <= n {
            f(u32::from_le_bytes([
                buf[i],
                buf[i + 1],
                buf[i + 2],
                buf[i + 3],
            ]));
            i += 4;
        }
        // Stash a 1-3 byte tail for the next read.
        while i < n {
            carry[carry_len] = buf[i];
            carry_len += 1;
            i += 1;
        }
    }
    debug_assert_eq!(carry_len, 0);
    Ok(())
}

/// A `Vec<u64>` held compressed across the peak window. Same codec choices as
/// [`CompressedU32`]. Used for `mat_addrs` and `mat_hprof_offsets`.
pub struct CompressedU64 {
    codec: Codec,
    blob: Vec<u8>,
    raw: Vec<u64>,
    len: usize,
}

impl CompressedU64 {
    /// Decompress the element at `target_idx` by streaming through the data. O(n).
    /// Use only for single-element lookups; prefer `restore()` for batch access.
    pub fn get_at(&self, target_idx: usize) -> io::Result<Option<u64>> {
        match self.codec {
            Codec::None => Ok(self.raw.get(target_idx).copied()),
            Codec::Deflate9 => {
                let mut r = flate2::read::DeflateDecoder::new(&self.blob[..]);
                let mut buf = [0u8; 8];
                let mut i = 0usize;
                loop {
                    match r.read_exact(&mut buf) {
                        Ok(()) => {
                            if i == target_idx {
                                return Ok(Some(u64::from_le_bytes(buf)));
                            }
                            i += 1;
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                            return Ok(None);
                        }
                        Err(e) => return Err(e),
                    }
                }
            }
        }
    }

    pub fn compress(v: &[u64], codec: Codec) -> io::Result<Self> {
        let len = v.len();
        match codec {
            Codec::None => Ok(Self {
                codec,
                blob: Vec::new(),
                raw: v.to_vec(),
                len,
            }),
            Codec::Deflate9 => {
                let bytes = unsafe { std::slice::from_raw_parts(v.as_ptr() as *const u8, len * 8) };
                let blob = deflate_compress(bytes)?;
                Ok(Self {
                    codec,
                    blob,
                    raw: Vec::new(),
                    len,
                })
            }
        }
    }

    pub fn restore(&self) -> io::Result<Vec<u64>> {
        match self.codec {
            Codec::None => Ok(self.raw.clone()),
            Codec::Deflate9 => {
                let bytes = deflate_decompress(&self.blob, self.len * 8)?;
                Ok(bytes
                    .chunks_exact(8)
                    .map(|c| u64::from_le_bytes(c.try_into().unwrap()))
                    .collect())
            }
        }
    }

    #[allow(dead_code)]
    pub fn held_bytes(&self) -> usize {
        match self.codec {
            Codec::None => self.raw.len() * 8,
            Codec::Deflate9 => self.blob.len(),
        }
    }
}

/// A `Vec<u8>` held compressed across a peak window. Used for `inb_data`
/// (vbyte-encoded inbound edge bytes) to avoid inflating the emit_outbound peak.
pub struct CompressedBytes {
    codec: Codec,
    blob: Vec<u8>,
    raw: Vec<u8>,
}

impl CompressedBytes {
    pub fn compress(v: Vec<u8>, codec: Codec) -> io::Result<Self> {
        match codec {
            Codec::None => Ok(Self {
                codec,
                blob: Vec::new(),
                raw: v,
            }),
            Codec::Deflate9 => {
                let blob = deflate_compress(&v)?;
                Ok(Self {
                    codec,
                    blob,
                    raw: Vec::new(),
                })
            }
        }
    }

    pub fn restore(self) -> io::Result<Vec<u8>> {
        match self.codec {
            Codec::None => Ok(self.raw),
            Codec::Deflate9 => deflate_decompress(&self.blob, self.blob.len() * 4),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_repetitive_deflate() {
        let mut v: Vec<u32> = Vec::new();
        for k in 0..1000u32 {
            for _ in 0..500 {
                v.push(k);
            }
        }
        let c = CompressedU32::compress(&v, Codec::Deflate9).unwrap();
        assert_eq!(c.restore().unwrap(), v);
        assert!(c.held_bytes() < v.len() * 4);
    }

    #[test]
    fn roundtrip_random_deflate() {
        let mut v: Vec<u32> = Vec::with_capacity(10_000);
        let mut state = 0x12345678u32;
        for _ in 0..10_000 {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            v.push(state);
        }
        let c = CompressedU32::compress(&v, Codec::Deflate9).unwrap();
        assert_eq!(c.restore().unwrap(), v);
    }

    #[test]
    fn roundtrip_none() {
        let v: Vec<u32> = vec![1, 2, 3, 0, u32::MAX, 42];
        let c = CompressedU32::compress(&v, Codec::None).unwrap();
        assert_eq!(c.restore().unwrap(), v);
        assert_eq!(c.held_bytes(), v.len() * 4);
    }

    #[test]
    fn empty() {
        let v: Vec<u32> = Vec::new();
        for codec in [Codec::None, Codec::Deflate9] {
            let c = CompressedU32::compress(&v, codec).unwrap();
            assert_eq!(c.restore().unwrap(), v);
        }
    }

    #[test]
    fn for_each_u32_matches_restore() {
        let mut v: Vec<u32> = Vec::with_capacity(100_000);
        let mut state = 0x9e3779b9u32;
        for _ in 0..100_000 {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            v.push(state);
        }
        v.extend_from_slice(&[0, u32::MAX, 1, 0]);
        for codec in [Codec::None, Codec::Deflate9] {
            let c = CompressedU32::compress(&v, codec).unwrap();
            let mut got: Vec<u32> = Vec::with_capacity(v.len());
            c.for_each_u32(|x| got.push(x)).unwrap();
            assert_eq!(got, v, "codec {codec:?}");
        }
    }

    #[test]
    fn codec_parse() {
        assert_eq!(Codec::parse("none"), Some(Codec::None));
        assert_eq!(Codec::parse("deflate9"), Some(Codec::Deflate9));
        assert_eq!(Codec::parse("deflate"), Some(Codec::Deflate9));
        assert_eq!(Codec::parse("zstd"), None);
    }

    #[test]
    fn test_get_at_u32() {
        let data: Vec<u32> = (0..1000).collect();
        let c = CompressedU32::compress(&data, Codec::Deflate9).unwrap();
        assert_eq!(c.get_at(0).unwrap(), Some(0));
        assert_eq!(c.get_at(999).unwrap(), Some(999));
        assert_eq!(c.get_at(1000).unwrap(), None);
        assert_eq!(c.slice_at(10, 15).unwrap(), vec![10u32, 11, 12, 13, 14]);
        assert_eq!(c.slice_at(0, 0).unwrap(), Vec::<u32>::new());
        assert_eq!(c.slice_at(998, 1001).unwrap(), vec![998u32, 999]);

        // Also test Codec::None path
        let c2 = CompressedU32::compress(&data, Codec::None).unwrap();
        assert_eq!(c2.get_at(500).unwrap(), Some(500));
        assert_eq!(c2.slice_at(5, 8).unwrap(), vec![5u32, 6, 7]);
    }

    #[test]
    fn test_get_at_u64() {
        let data: Vec<u64> = (0..500u64).map(|i| i * 1_000_000).collect();
        let c = CompressedU64::compress(&data, Codec::Deflate9).unwrap();
        assert_eq!(c.get_at(0).unwrap(), Some(0));
        assert_eq!(c.get_at(499).unwrap(), Some(499 * 1_000_000));
        assert_eq!(c.get_at(500).unwrap(), None);

        // Also test Codec::None path
        let c2 = CompressedU64::compress(&data, Codec::None).unwrap();
        assert_eq!(c2.get_at(100).unwrap(), Some(100 * 1_000_000));
        assert_eq!(c2.get_at(500).unwrap(), None);
    }
}

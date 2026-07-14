//! Compressed holders for the large "cold" per-object arrays (shallow,
//! class_idx) that sit idle in RAM across the rpo -> inbound -> dominator peak
//! window. Compress right after they are built, hold the small blob across the
//! peak, and restore the full `Vec<u32>` only when a consumer needs random
//! access. deflate9 (flate2) is used: on the 34 GB dump it frees ~all of each
//! 2 GB array (blob ~33 MB) in ~32 s; higher-ratio codecs shrink the blob by a
//! further <0.1 % of the peak for 5-10x the compress time (see plan Step 0).

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
    /// Parse a codec name (`none`, `deflate9`/`deflate`); test-only A/B helper.
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

/// Deflate raw bytes at max level (Deflate9). Public thin wrapper so other
/// modules (e.g. `RootPathCtx`) can hold arbitrary byte blobs compressed with
/// the same codec used for the cold per-object arrays.
pub fn deflate_bytes(raw: &[u8]) -> io::Result<Vec<u8>> {
    deflate_compress(raw)
}

/// Inflate a `deflate_bytes` blob back to raw bytes; `cap` pre-sizes the output.
pub fn inflate_bytes(blob: &[u8], cap: usize) -> io::Result<Vec<u8>> {
    deflate_decompress(blob, cap)
}

/// A `Vec<u32>` held compressed across the peak window, restorable losslessly.
///
/// With `Codec::None` this keeps the live `Vec<u32>` unchanged (no free); with
/// `Codec::Deflate9` it holds only a deflate blob of the LE bytes and the
/// original element count.
pub struct CompressedU32 {
    codec: Codec,
    /// deflate blob (Deflate9) or raw LE bytes are NOT stored here for None.
    blob: Vec<u8>,
    /// Live copy for the None codec (empty for Deflate9).
    raw: Vec<u32>,
    len: usize,
}

impl CompressedU32 {
    /// Compress `v` under `codec`. For `None`, takes ownership-free copy is
    /// avoided by cloning only when needed; callers empty the source Vec after.
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
    pub fn restore(&self) -> io::Result<Vec<u32>> {
        match self.codec {
            Codec::None => Ok(self.raw.clone()),
            Codec::Deflate9 => {
                let bytes = deflate_decompress(&self.blob, self.len * 4)?;
                debug_assert_eq!(bytes.len(), self.len * 4);
                Ok(bytes
                    .chunks_exact(4)
                    .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                    .collect())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_repetitive_deflate() {
        // Long runs of identical values (like class_idx/shallow).
        let mut v: Vec<u32> = Vec::new();
        for k in 0..1000u32 {
            for _ in 0..500 {
                v.push(k);
            }
        }
        let c = CompressedU32::compress(&v, Codec::Deflate9).unwrap();
        assert_eq!(c.restore().unwrap(), v);
        // Repetitive data must compress well below raw.
        assert!(c.held_bytes() < v.len() * 4);
    }

    #[test]
    fn roundtrip_random_deflate() {
        let mut v: Vec<u32> = Vec::with_capacity(10_000);
        let mut state = 0x12345678u32;
        for _ in 0..10_000 {
            // xorshift PRNG (deterministic)
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
    fn codec_parse() {
        assert_eq!(Codec::parse("none"), Some(Codec::None));
        assert_eq!(Codec::parse("deflate9"), Some(Codec::Deflate9));
        assert_eq!(Codec::parse("deflate"), Some(Codec::Deflate9));
        assert_eq!(Codec::parse("zstd"), None);
    }
}

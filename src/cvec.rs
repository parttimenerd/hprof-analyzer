//! Compressed holders for the large "cold" per-object arrays (shallow,
//! class_idx) that sit idle in RAM across the rpo -> inbound -> dominator peak
//! window. Compress right after they are built, hold the small blob across the
//! peak, and restore the full `Vec<u32>` only when a consumer needs random
//! access. Zstd level 3 is used by default: on large dumps it frees each ~2 GB
//! array (blob ~33 MB) in ~5 s; deflate9 (flate2) is kept as a fallback codec.

use std::io::{self, Read, Write};

/// Which codec to use across the peak window.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Codec {
    /// No compression: keep the live Vec (no RSS win; A/B escape hatch).
    None,
    /// deflate at max level (flate2 Compression::best()).
    Deflate9,
    /// zstd at level 3 — fast compress, good ratio, fast decompress.
    Zstd3,
}

impl Codec {
    /// Parse a codec name; test-only A/B helper.
    #[cfg(test)]
    pub fn parse(s: &str) -> Option<Codec> {
        match s {
            "none" => Some(Codec::None),
            "deflate9" | "deflate" => Some(Codec::Deflate9),
            "zstd" | "zstd3" => Some(Codec::Zstd3),
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

fn zstd_compress(raw: &[u8]) -> io::Result<Vec<u8>> {
    zstd::encode_all(raw, 3).map_err(io::Error::other)
}

fn zstd_decompress(blob: &[u8], cap: usize) -> io::Result<Vec<u8>> {
    let mut out = Vec::with_capacity(cap);
    zstd::stream::copy_decode(blob, &mut out).map_err(io::Error::other)?;
    Ok(out)
}

/// A `Vec<u32>` held compressed across the peak window, restorable losslessly.
///
/// With `Codec::None` this keeps the live `Vec<u32>` unchanged (no free); with
/// `Codec::Deflate9`/`Codec::Zstd3` it holds only a compressed blob of the LE
/// bytes and the original element count.
pub struct CompressedU32 {
    codec: Codec,
    /// Compressed blob (Deflate9/Zstd3) or empty (None).
    blob: Vec<u8>,
    /// Live copy for the None codec (empty for compressed codecs).
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
            Codec::Deflate9 | Codec::Zstd3 => {
                let mut bytes = Vec::with_capacity(len * 4);
                for &x in v {
                    bytes.extend_from_slice(&x.to_le_bytes());
                }
                let blob = if codec == Codec::Zstd3 {
                    zstd_compress(&bytes)?
                } else {
                    deflate_compress(&bytes)?
                };
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
            Codec::Zstd3 => {
                let bytes = zstd_decompress(&self.blob, self.len * 4)?;
                debug_assert_eq!(bytes.len(), self.len * 4);
                Ok(bytes
                    .chunks_exact(4)
                    .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                    .collect())
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
            Codec::Zstd3 => {
                let decoder = zstd::stream::Decoder::new(&self.blob[..])?;
                stream_u32s(decoder, &mut f)
            }
        }
    }

    /// Bytes currently held (blob for compressed codecs, raw*4 for None).
    #[allow(dead_code)]
    pub fn held_bytes(&self) -> usize {
        match self.codec {
            Codec::None => self.raw.len() * 4,
            Codec::Deflate9 | Codec::Zstd3 => self.blob.len(),
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
            f(u32::from_le_bytes([buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]));
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
    fn roundtrip_repetitive_zstd() {
        let mut v: Vec<u32> = Vec::new();
        for k in 0..1000u32 {
            for _ in 0..500 {
                v.push(k);
            }
        }
        let c = CompressedU32::compress(&v, Codec::Zstd3).unwrap();
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
    fn roundtrip_random_zstd() {
        let mut v: Vec<u32> = Vec::with_capacity(10_000);
        let mut state = 0x12345678u32;
        for _ in 0..10_000 {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            v.push(state);
        }
        let c = CompressedU32::compress(&v, Codec::Zstd3).unwrap();
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
        for codec in [Codec::None, Codec::Deflate9, Codec::Zstd3] {
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
        for codec in [Codec::None, Codec::Deflate9, Codec::Zstd3] {
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
        assert_eq!(Codec::parse("zstd"), Some(Codec::Zstd3));
        assert_eq!(Codec::parse("zstd3"), Some(Codec::Zstd3));
    }
}

use std::io::{self, Read, Write};

use crate::cvec::Codec;
use crate::vbyte;

/// Maps heap object addresses (u64) to dense integer indices via sorted binary search.
/// Push all addresses, call sort_and_dedup(), then use index_of() for O(log n) lookup.
pub struct IdMap {
    addrs: Vec<u64>,
}

impl IdMap {
    pub fn new() -> Self {
        Self { addrs: Vec::new() }
    }

    pub fn with_capacity(cap: usize) -> Self {
        Self { addrs: Vec::with_capacity(cap) }
    }

    pub fn push(&mut self, addr: u64) {
        self.addrs.push(addr);
    }

    pub fn sort_and_dedup(&mut self) {
        self.addrs.sort_unstable();
        self.addrs.dedup();
    }

    pub fn len(&self) -> usize {
        self.addrs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.addrs.is_empty()
    }

    /// Returns None if addr not found (not a heap object or null reference).
    pub fn index_of(&self, addr: u64) -> Option<usize> {
        self.addrs.binary_search(&addr).ok()
    }

    pub fn addr_at(&self, i: usize) -> u64 {
        self.addrs[i]
    }

    /// Compress the sorted addr array under `codec` into a self-describing blob,
    /// returning (blob, element_count). For `Deflate9` the addrs are vbyte
    /// delta-encoded (small gaps, ~8x) then deflated; for `None` the raw LE u64
    /// bytes are stored uncompressed. `from_compressed` reverses either.
    pub fn compress(&self, codec: Codec) -> io::Result<(Vec<u8>, usize)> {
        let len = self.addrs.len();
        match codec {
            Codec::None => {
                let mut out = Vec::with_capacity(len * 8);
                for &a in &self.addrs {
                    out.extend_from_slice(&a.to_le_bytes());
                }
                Ok((out, len))
            }
            Codec::Deflate9 => {
                let mut vb = Vec::with_capacity(len * 2);
                vbyte::encode_delta_u64(&self.addrs, &mut vb);
                let mut e = flate2::write::DeflateEncoder::new(
                    Vec::new(),
                    flate2::Compression::best(),
                );
                e.write_all(&vb)?;
                let blob = e.finish()?;
                Ok((blob, len))
            }
        }
    }

    /// Rebuild an IdMap from a blob produced by `compress` with the same codec.
    pub fn from_compressed(blob: &[u8], len: usize, codec: Codec) -> io::Result<Self> {
        match codec {
            Codec::None => {
                debug_assert_eq!(blob.len(), len * 8);
                let addrs = blob
                    .chunks_exact(8)
                    .map(|c| u64::from_le_bytes([
                        c[0], c[1], c[2], c[3], c[4], c[5], c[6], c[7],
                    ]))
                    .collect();
                Ok(Self { addrs })
            }
            Codec::Deflate9 => {
                let mut d = flate2::read::DeflateDecoder::new(blob);
                let mut vb = Vec::new();
                d.read_to_end(&mut vb)?;
                let addrs = vbyte::decode_delta_u64(&vb, len);
                debug_assert_eq!(addrs.len(), len);
                Ok(Self { addrs })
            }
        }
    }
}

impl Default for IdMap {
    fn default() -> Self { Self::new() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_and_lookup() {
        let mut m = IdMap::new();
        m.push(0x300);
        m.push(0x100);
        m.push(0x200);
        m.push(0x100); // duplicate
        m.sort_and_dedup();
        assert_eq!(m.len(), 3);
        assert_eq!(m.index_of(0x100), Some(0));
        assert_eq!(m.index_of(0x200), Some(1));
        assert_eq!(m.index_of(0x300), Some(2));
        assert_eq!(m.index_of(0x400), None);
    }

    #[test]
    fn empty_map() {
        let mut m = IdMap::new();
        m.sort_and_dedup();
        assert_eq!(m.len(), 0);
        assert!(m.is_empty());
        assert_eq!(m.index_of(42), None);
    }

    #[test]
    fn single_element() {
        let mut m = IdMap::new();
        m.push(999);
        m.sort_and_dedup();
        assert_eq!(m.len(), 1);
        assert_eq!(m.index_of(999), Some(0));
        assert_eq!(m.index_of(0), None);
    }

    #[test]
    fn addr_roundtrip() {
        let mut m = IdMap::new();
        let addrs = vec![0x10u64, 0x30, 0x50, 0x20, 0x40];
        for &a in &addrs { m.push(a); }
        m.sort_and_dedup();
        // sorted order: 0x10, 0x20, 0x30, 0x40, 0x50
        assert_eq!(m.addr_at(0), 0x10);
        assert_eq!(m.addr_at(2), 0x30);
        assert_eq!(m.addr_at(4), 0x50);
    }
}

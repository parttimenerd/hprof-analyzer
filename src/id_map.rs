use std::io::{self, Read, Write};

use crate::cvec::Codec;
use crate::vbyte;

/// Maps heap object addresses (u64) to dense integer indices.
///
/// Two-level layout: instead of one sorted `Vec<u64>` (8 bytes/entry), the
/// sorted addresses are stored as `Vec<u32>` offsets from a small number of
/// 64-bit block bases. A new block starts whenever the next sorted address is
/// 2^32 or more beyond the current block base, so every offset fits in u32. Real
/// heap dumps span a handful of regions => a handful of blocks, so this halves
/// the per-object cost (4 bytes vs 8) — the dominant array at 500M+ objects.
///
/// Usage: push all addresses, call sort_and_dedup(), then index_of()/addr_at().
pub struct IdMap {
    /// Ascending base address of each block (block b covers
    /// offsets[block_start[b]..block_start[b+1]], each addr = base + offset).
    block_base: Vec<u64>,
    /// Start index into `offsets` for each block; len == block_base.len()+1,
    /// trailing sentinel == offsets.len() so block b spans
    /// block_start[b]..block_start[b+1]. Empty map => [0].
    block_start: Vec<u32>,
    /// Per-element offset from its block base (addr - block_base[block_of(i)]).
    offsets: Vec<u32>,
    /// Build-time staging of raw addresses before sort_and_dedup().
    staging: Vec<u64>,
}

const BLOCK_SPAN: u64 = 1u64 << 32;

#[allow(dead_code)]
impl IdMap {
    pub fn new() -> Self {
        Self {
            block_base: Vec::new(),
            block_start: Vec::new(),
            offsets: Vec::new(),
            staging: Vec::new(),
        }
    }

    pub fn with_capacity(cap: usize) -> Self {
        Self {
            block_base: Vec::new(),
            block_start: Vec::new(),
            offsets: Vec::new(),
            staging: Vec::with_capacity(cap),
        }
    }

    pub fn push(&mut self, addr: u64) {
        self.staging.push(addr);
    }

    /// Build the two-level index from staged addresses (sorted + deduped).
    /// Frees the u64 staging Vec afterward so only the u32 offsets remain.
    pub fn sort_and_dedup(&mut self) {
        self.staging.sort_unstable();
        self.staging.dedup();
        self.build_from_sorted();
    }

    /// Append one address, which MUST be strictly greater than every address
    /// pushed so far (callers feed already-sorted, deduped addresses). Builds
    /// the two-level structure incrementally so the 8-byte `staging` Vec is
    /// never allocated — only the u32 `offsets` grow. Call `finalize_sorted`
    /// once all addresses are pushed. Reserve capacity first via
    /// `reserve_offsets` to avoid repeated reallocs.
    #[inline]
    pub fn push_sorted_addr(&mut self, addr: u64) {
        debug_assert!(
            self.staging.is_empty(),
            "cannot mix push and push_sorted_addr"
        );
        if self.block_base.is_empty() {
            // First address opens the first block.
            self.block_base.push(addr);
            self.block_start.push(0);
            self.offsets.push(0);
            return;
        }
        let mut base = *self.block_base.last().unwrap();
        debug_assert!(
            addr > self.addr_at(self.offsets.len() - 1),
            "push_sorted_addr not ascending"
        );
        if addr - base >= BLOCK_SPAN {
            base = addr;
            self.block_base.push(base);
            self.block_start.push(self.offsets.len() as u32);
        }
        self.offsets.push((addr - base) as u32);
    }

    /// Reserve capacity for `n` offsets before a run of `push_sorted_addr`.
    pub fn reserve_offsets(&mut self, n: usize) {
        self.offsets.reserve(n);
    }

    /// Close the block table after a run of `push_sorted_addr` (adds the
    /// trailing sentinel). No-op for the empty map beyond the sentinel.
    pub fn finalize_sorted(&mut self) {
        if self.block_start.is_empty() {
            self.block_start.push(0);
        } else {
            self.block_start.push(self.offsets.len() as u32);
        }
    }

    /// Internal: build block_base/block_start/offsets from the sorted, deduped
    /// `staging` Vec, then free `staging`.
    fn build_from_sorted(&mut self) {
        let len = self.staging.len();
        self.block_base = Vec::new();
        self.block_start = Vec::new();
        self.offsets = Vec::with_capacity(len);
        if len == 0 {
            self.block_start.push(0);
            self.staging = Vec::new();
            return;
        }
        let mut base = self.staging[0];
        self.block_base.push(base);
        self.block_start.push(0);
        for (i, &a) in self.staging.iter().enumerate() {
            if a - base >= BLOCK_SPAN {
                base = a;
                self.block_base.push(base);
                self.block_start.push(i as u32);
            }
            self.offsets.push((a - base) as u32);
        }
        self.block_start.push(len as u32); // sentinel
        self.staging = Vec::new();
    }

    pub fn len(&self) -> usize {
        self.offsets.len()
    }

    pub fn is_empty(&self) -> bool {
        self.offsets.is_empty()
    }

    /// Returns None if addr not found (not a heap object or null reference).
    pub fn index_of(&self, addr: u64) -> Option<usize> {
        if self.block_base.is_empty() || addr < self.block_base[0] {
            return None;
        }
        // Greatest block base <= addr.
        let b = self.block_base.partition_point(|&base| base <= addr) - 1;
        let delta = addr - self.block_base[b];
        if delta >= BLOCK_SPAN {
            return None; // addr sits in the gap after block b, before b+1's base
        }
        let d = delta as u32;
        let lo = self.block_start[b] as usize;
        let hi = self.block_start[b + 1] as usize;
        match self.offsets[lo..hi].binary_search(&d) {
            Ok(pos) => Some(lo + pos),
            Err(_) => None,
        }
    }

    pub fn addr_at(&self, i: usize) -> u64 {
        // Block whose start index is the greatest <= i.
        let b = self.block_start.partition_point(|&s| (s as usize) <= i) - 1;
        self.block_base[b] + self.offsets[i] as u64
    }

    /// Compress the sorted addrs into a self-describing blob (same format as the
    /// prior single-Vec layout: absolute addrs, delta-vbyte then deflate for
    /// Deflate9, or raw LE u64 for None), returning (blob, element_count).
    pub fn compress(&self, codec: Codec) -> io::Result<(Vec<u8>, usize)> {
        let len = self.len();
        match codec {
            Codec::None => {
                let mut out = Vec::with_capacity(len * 8);
                for i in 0..len {
                    out.extend_from_slice(&self.addr_at(i).to_le_bytes());
                }
                Ok((out, len))
            }
            Codec::Deflate9 => {
                // Reconstruct the absolute sorted addrs for delta encoding.
                let mut addrs: Vec<u64> = Vec::with_capacity(len);
                for i in 0..len {
                    addrs.push(self.addr_at(i));
                }
                let mut vb = Vec::with_capacity(len * 2);
                vbyte::encode_delta_u64(&addrs, &mut vb);
                let mut e =
                    flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::best());
                e.write_all(&vb)?;
                let blob = e.finish()?;
                Ok((blob, len))
            }
        }
    }

    /// Rebuild an IdMap from a blob produced by `compress` with the same codec.
    ///
    /// Streams addresses directly into the two-level structure via
    /// `push_sorted_addr` — the 4.1GB (@514M) u64 `addrs`/`staging` Vec is
    /// NEVER materialized, only the 2.05GB u32 `offsets` grow. This was the
    /// binding global RSS peak: the old path decoded a full u64 Vec AND then
    /// held it as `staging` while `build_from_sorted` grew `offsets`, a
    /// ~6.2GB transient on top of the live rpo arrays at inbound-start.
    pub fn from_compressed(blob: &[u8], len: usize, codec: Codec) -> io::Result<Self> {
        let mut m = IdMap::new();
        m.reserve_offsets(len);
        match codec {
            Codec::None => {
                debug_assert_eq!(blob.len(), len * 8);
                for c in blob.chunks_exact(8) {
                    let addr = u64::from_le_bytes([c[0], c[1], c[2], c[3], c[4], c[5], c[6], c[7]]);
                    m.push_sorted_addr(addr);
                }
            }
            Codec::Deflate9 => {
                // The deflate output is the vbyte-delta stream (small relative
                // to the 4.1GB decoded addresses). Walk it delta-by-delta and
                // accumulate the running absolute address.
                let mut d = flate2::read::DeflateDecoder::new(blob);
                let mut vb = Vec::new();
                d.read_to_end(&mut vb)?;
                let mut prev = 0u64;
                let mut i = 0usize;
                let mut pushed = 0usize;
                while i < vb.len() && pushed < len {
                    let (delta, consumed) = vbyte::decode_one_u64(&vb[i..]);
                    prev += delta;
                    m.push_sorted_addr(prev);
                    i += consumed;
                    pushed += 1;
                }
                debug_assert_eq!(pushed, len);
            }
        }
        m.finalize_sorted();
        Ok(m)
    }
}

impl Default for IdMap {
    fn default() -> Self {
        Self::new()
    }
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
        for &a in &addrs {
            m.push(a);
        }
        m.sort_and_dedup();
        assert_eq!(m.addr_at(0), 0x10);
        assert_eq!(m.addr_at(2), 0x30);
        assert_eq!(m.addr_at(4), 0x50);
    }

    #[test]
    fn multi_block_far_apart() {
        // Addresses spanning >2^32 force multiple blocks.
        let mut m = IdMap::new();
        let a0 = 0x1000u64;
        let a1 = 0x1000u64 + (1u64 << 33); // new block
        let a2 = a1 + 0x40; // same block as a1
        let a3 = a1 + (1u64 << 34); // another new block
        for &a in &[a3, a0, a2, a1] {
            m.push(a);
        } // unsorted on purpose
        m.sort_and_dedup();
        assert_eq!(m.len(), 4);
        // sorted: a0, a1, a2, a3
        assert_eq!(m.index_of(a0), Some(0));
        assert_eq!(m.index_of(a1), Some(1));
        assert_eq!(m.index_of(a2), Some(2));
        assert_eq!(m.index_of(a3), Some(3));
        assert_eq!(m.addr_at(0), a0);
        assert_eq!(m.addr_at(1), a1);
        assert_eq!(m.addr_at(2), a2);
        assert_eq!(m.addr_at(3), a3);
        // A gap address between blocks must not resolve.
        assert_eq!(m.index_of(a0 + 0x8), None);
        assert_eq!(m.index_of(a1 - 0x8), None);
    }

    #[test]
    fn push_sorted_incremental_matches_batch() {
        // Build the same set two ways and confirm identical structure/results.
        let addrs_sorted: Vec<u64> = {
            let base = 0x7f00_0000_0000u64;
            let mut v = Vec::new();
            for k in 0..6u64 {
                v.push(base + k * (1u64 << 33) + 0x10);
                v.push(base + k * (1u64 << 33) + 0x20);
                v.push(base + k * (1u64 << 33) + 0x30);
            }
            v.sort_unstable();
            v.dedup();
            v
        };

        let mut batch = IdMap::new();
        for &a in &addrs_sorted {
            batch.push(a);
        }
        batch.sort_and_dedup();

        let mut incr = IdMap::new();
        incr.reserve_offsets(addrs_sorted.len());
        for &a in &addrs_sorted {
            incr.push_sorted_addr(a);
        }
        incr.finalize_sorted();

        assert_eq!(incr.len(), batch.len());
        for i in 0..batch.len() {
            let a = batch.addr_at(i);
            assert_eq!(incr.addr_at(i), a);
            assert_eq!(incr.index_of(a), Some(i));
        }
        // Gap + out-of-range must not resolve on the incremental map.
        assert_eq!(incr.index_of(addrs_sorted[0] - 1), None);
        assert_eq!(incr.index_of(*addrs_sorted.last().unwrap() + 1), None);
    }

    #[test]
    fn push_sorted_empty() {
        let mut m = IdMap::new();
        m.finalize_sorted();
        assert_eq!(m.len(), 0);
        assert!(m.is_empty());
        assert_eq!(m.index_of(1), None);
    }

    #[test]
    fn compress_roundtrip_multiblock() {
        let mut m = IdMap::new();
        let base = 0x7f00_0000_0000u64;
        for k in 0..5u64 {
            m.push(base + k * (1u64 << 33) + 0x10);
            m.push(base + k * (1u64 << 33) + 0x20);
        }
        m.sort_and_dedup();
        let n = m.len();
        for codec in [Codec::None, Codec::Deflate9] {
            let (blob, len) = m.compress(codec).unwrap();
            let m2 = IdMap::from_compressed(&blob, len, codec).unwrap();
            assert_eq!(m2.len(), n);
            for i in 0..n {
                let a = m.addr_at(i);
                assert_eq!(m2.addr_at(i), a);
                assert_eq!(m2.index_of(a), Some(i));
            }
        }
    }
}

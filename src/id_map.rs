//! Maps 64-bit HPROF object IDs to dense 0-based node indices.
//!
//! The mapping is a compact two-level structure (u32 offsets from a few 64-bit
//! block bases) rather than a raw `Vec<u64>`, halving per-object cost at 500M+
//! objects. It also (de)serializes to a compressed blob so the u64 address list
//! is never materialized. This module is on the peak-RSS-critical path: its
//! build/compress steps are among the binding memory peaks on large dumps.

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

/// Interpolation search over a strictly-ascending, unique `u32` slice.
/// Returns `Some(i)` such that `slice[i] == d`, or `None` if absent.
///
/// JVM heap addresses within a block are allocated quasi-linearly, so the
/// sorted offsets array is nearly uniformly spaced. Interpolation search
/// uses `pos ≈ lo + (hi-lo)*(d-slice[lo])/(slice[hi]-slice[lo])` to land
/// in 1-3 probes vs. ~29 for binary search. After a few interpolation steps
/// we fall back to branchless binary search once the range is small enough
/// to fit in cache.
///
/// The fallback binary search is the Khuong-Morin branchless bisection with
/// prefetching for the final narrowing.
#[inline]
fn search_offsets(slice: &[u32], d: u32) -> Option<usize> {
    let n = slice.len();
    if n == 0 {
        return None;
    }
    // Fast path for single-element slice.
    if n == 1 {
        return if unsafe { *slice.get_unchecked(0) } == d {
            Some(0)
        } else {
            None
        };
    }

    let mut lo = 0usize;
    let mut hi = n - 1;

    // Bounds check: d outside [slice[lo], slice[hi]] → absent.
    // SAFETY: lo < hi < n so both indices are valid.
    let lo_val = unsafe { *slice.get_unchecked(lo) };
    let hi_val = unsafe { *slice.get_unchecked(hi) };
    if d < lo_val || d > hi_val {
        return None;
    }

    // Interpolation phase: up to 4 probes. Each probe narrows the range
    // dramatically on uniformly-distributed data (typical JVM allocation).
    // We stop when the range is ≤ 128 entries (fits in a few cache lines)
    // and hand off to branchless binary search.
    for _ in 0..4 {
        let range = hi - lo;
        if range <= 128 {
            break;
        }
        // SAFETY: lo and hi are both < n throughout.
        let lo_v = unsafe { *slice.get_unchecked(lo) } as u64;
        let hi_v = unsafe { *slice.get_unchecked(hi) } as u64;
        if hi_v == lo_v {
            break; // all remaining elements equal — can't interpolate
        }
        // Interpolation probe. Cast to u64 to avoid overflow.
        // Formula: lo + floor(range * (d - lo_v) / (hi_v - lo_v))
        // Clamped to [lo+1, hi-1] so we always make progress.
        let offset = ((range as u64 * (d as u64 - lo_v) / (hi_v - lo_v)) as usize).min(range - 1);
        let mid = (lo + offset).max(lo + 1).min(hi - 1);
        let mid_v = unsafe { *slice.get_unchecked(mid) };
        if mid_v == d {
            return Some(mid);
        } else if mid_v < d {
            lo = mid + 1;
        } else {
            hi = mid.saturating_sub(1);
        }
        if lo > hi {
            return None;
        }
        // Re-check bounds after narrowing.
        let lv = unsafe { *slice.get_unchecked(lo) };
        let hv = unsafe { *slice.get_unchecked(hi) };
        if d < lv || d > hv {
            return None;
        }
    }

    // Branchless binary search (Khuong-Morin) with prefetching over [lo, hi].
    // At most 128 elements remain so we issue at most 7 iterations.
    let sub = &slice[lo..=hi];
    let sub_n = sub.len();
    let mut base = 0usize;
    let mut len = sub_n;
    while len > 1 {
        let half = len / 2;
        let mid = base + half;
        let q = (len - half) / 2;
        let ptr = sub.as_ptr();
        #[cfg(target_arch = "x86_64")]
        unsafe {
            use core::arch::x86_64::{_MM_HINT_T0, _mm_prefetch};
            _mm_prefetch(ptr.add(base + q) as *const i8, _MM_HINT_T0);
            _mm_prefetch(ptr.add(mid + q) as *const i8, _MM_HINT_T0);
        }
        #[cfg(target_arch = "aarch64")]
        unsafe {
            core::arch::asm!(
                "prfm pldl1keep, [{p}]",
                p = in(reg) ptr.add(base + q),
                options(nostack, readonly)
            );
            core::arch::asm!(
                "prfm pldl1keep, [{p}]",
                p = in(reg) ptr.add(mid + q),
                options(nostack, readonly)
            );
        }
        let take_upper = unsafe { *sub.get_unchecked(mid) } < d;
        base = if take_upper { mid } else { base };
        len -= half;
    }
    let pos = base + (unsafe { *sub.get_unchecked(base) } < d) as usize;
    if pos < sub_n && unsafe { *sub.get_unchecked(pos) } == d {
        Some(lo + pos)
    } else {
        None
    }
}

#[allow(dead_code)]
impl IdMap {
    /// Empty map; push addresses then call `sort_and_dedup`.
    pub fn new() -> Self {
        Self {
            block_base: Vec::new(),
            block_start: Vec::new(),
            offsets: Vec::new(),
            staging: Vec::new(),
        }
    }

    /// Empty map with `cap` staging slots preallocated for `push`.
    pub fn with_capacity(cap: usize) -> Self {
        Self {
            block_base: Vec::new(),
            block_start: Vec::new(),
            offsets: Vec::new(),
            staging: Vec::with_capacity(cap),
        }
    }

    /// Stage one raw address; order-independent (sorted later by `sort_and_dedup`).
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

    /// Number of distinct addresses in the map.
    pub fn len(&self) -> usize {
        self.offsets.len()
    }

    /// True when the map holds no addresses.
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
        search_offsets(&self.offsets[lo..hi], d).map(|pos| lo + pos)
    }

    /// Reconstruct the address stored at dense index `i` (inverse of `index_of`).
    pub fn addr_at(&self, i: usize) -> u64 {
        // Block whose start index is the greatest <= i.
        let b = self.block_start.partition_point(|&s| (s as usize) <= i) - 1;
        self.block_base[b] + self.offsets[i] as u64
    }

    /// Compress the sorted addrs into a self-describing blob (same format as the
    /// prior single-Vec layout: absolute addrs, delta-vbyte then compressed for
    /// Deflate9/Zstd3, or raw LE u64 for None), returning (blob, element_count).
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
            Codec::Deflate9 | Codec::Zstd3 => {
                // Stream the sorted absolute addrs directly out of the block
                // structure and delta-vbyte-encode on the fly. Walking the
                // blocks in order yields the exact same globally-sorted address
                // sequence addr_at(0..len) would, but WITHOUT reconstructing the
                // 4.1GB `addrs: Vec<u64>` (@514M) that used to be the binding
                // compress-cold peak on top of the ~13GB fwd CSR.
                let mut vb: Vec<u8> = Vec::new();
                let mut prev = 0u64;
                for b in 0..self.block_base.len() {
                    let base = self.block_base[b];
                    let lo = self.block_start[b] as usize;
                    let hi = self.block_start[b + 1] as usize;
                    for &off in &self.offsets[lo..hi] {
                        let addr = base + off as u64;
                        vbyte::encode_u64(addr - prev, &mut vb);
                        prev = addr;
                    }
                }
                let blob = if codec == Codec::Zstd3 {
                    zstd::encode_all(&vb[..], 3).map_err(io::Error::other)?
                } else {
                    let mut e =
                        flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::best());
                    e.write_all(&vb)?;
                    e.finish()?
                };
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
            Codec::Deflate9 | Codec::Zstd3 => {
                // The compressed output is the vbyte-delta stream (small relative
                // to the 4.1GB decoded addresses). Walk it delta-by-delta and
                // accumulate the running absolute address.
                let vb = if codec == Codec::Zstd3 {
                    zstd::decode_all(blob).map_err(io::Error::other)?
                } else {
                    let mut d = flate2::read::DeflateDecoder::new(blob);
                    let mut vb = Vec::new();
                    d.read_to_end(&mut vb)?;
                    vb
                };
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

// ── Direct-mapped index cache ──────────────────────────────────────────────

/// Capacity: 16384 slots. 16384 × (8 + 4) = 192 KB — fits in L2/L3.
const IC_BITS: usize = 14;
const IC_SIZE: usize = 1 << IC_BITS;

/// Direct-mapped cache over [`IdMap::index_of`] for hot scanning loops.
///
/// Heap graphs have high temporal locality on class-object addresses (one per
/// class, referenced by every instance of that class), so a tiny cache
/// eliminates most of the random-DRAM penalty on the dominant object-edge
/// workload. Miss → call `id_map.index_of` and store; hit → return cached.
///
/// The slot is selected by `(addr ^ (addr >> 20)) & (IC_SIZE - 1)`, mixing
/// high and low bits to distribute nearby addresses across slots.
/// Sentinel: `key == 0` means empty (valid HPROF addrs are always non-zero).
pub struct IndexCache {
    keys: Box<[u64; IC_SIZE]>,
    /// `u32::MAX` encodes `None` (absent from IdMap); otherwise the dense idx.
    vals: Box<[u32; IC_SIZE]>,
}

impl IndexCache {
    /// Allocate a zeroed cache (all slots empty — key 0 = sentinel).
    pub fn new() -> Self {
        Self {
            keys: Box::new([0u64; IC_SIZE]),
            vals: Box::new([0u32; IC_SIZE]),
        }
    }

    /// Lookup `addr` in `id_map`, consulting the cache first.
    /// Returns `Some(dense_index)` or `None` (absent).
    #[inline(always)]
    pub fn index_of(&mut self, id_map: &IdMap, addr: u64) -> Option<usize> {
        if addr == 0 {
            return None;
        }
        let slot = ((addr ^ (addr >> 20)) as usize) & (IC_SIZE - 1);
        // SAFETY: slot < IC_SIZE by construction.
        let k = unsafe { *self.keys.get_unchecked(slot) };
        if k == addr {
            let v = unsafe { *self.vals.get_unchecked(slot) };
            return if v == u32::MAX {
                None
            } else {
                Some(v as usize)
            };
        }
        // Miss: call through and populate the slot.
        let result = id_map.index_of(addr);
        unsafe {
            *self.keys.get_unchecked_mut(slot) = addr;
            *self.vals.get_unchecked_mut(slot) = result.map(|i| i as u32).unwrap_or(u32::MAX);
        }
        result
    }
}

impl Default for IndexCache {
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

    // Single-block boundary coverage: every inserted addr round-trips, and
    // absent addrs (below base, above max, in interior gaps) return None. All
    // these addrs fit in one 4 GB span so they exercise the single-block path.
    #[test]
    fn single_block_boundaries() {
        let base = 0x7f00_0000_0000u64;
        let addrs: Vec<u64> = vec![base, base + 0x10, base + 0x1000, base + 0x1_0000];
        let mut m = IdMap::new();
        for &a in &addrs {
            m.push(a);
        }
        m.sort_and_dedup();
        assert_eq!(m.block_base.len(), 1, "all addrs must land in one block");
        for (i, &a) in addrs.iter().enumerate() {
            assert_eq!(m.index_of(a), Some(i));
            assert_eq!(m.addr_at(i), a);
        }
        // below base, above max, interior gaps -> None
        assert_eq!(m.index_of(base - 1), None);
        assert_eq!(m.index_of(base + 0x1_0001), None);
        assert_eq!(m.index_of(base + 1), None);
        assert_eq!(m.index_of(base + 0x800), None);
    }

    // The single-block result must equal what a from-scratch general-path
    // computation yields. We build the SAME address set once as a single block
    // (all within 2^32) and confirm index_of/addr_at agree with a naive
    // sorted-vec binary search reference.
    #[test]
    fn single_block_matches_naive_reference() {
        let base = 0x1234_0000_0000u64;
        let mut addrs: Vec<u64> = (0..500u64).map(|k| base + k.wrapping_mul(0x9E37)).collect();
        addrs.sort_unstable();
        addrs.dedup();
        let mut m = IdMap::new();
        for &a in &addrs {
            m.push(a);
        }
        m.sort_and_dedup();
        assert_eq!(m.block_base.len(), 1);
        for (i, &a) in addrs.iter().enumerate() {
            assert_eq!(m.index_of(a), Some(i));
            assert_eq!(m.addr_at(i), a);
        }
        // A handful of non-members must resolve to None.
        for probe in [base - 5, base + 0x9E37 / 2, addrs.last().unwrap() + 7] {
            let want = addrs.binary_search(&probe).ok();
            assert_eq!(m.index_of(probe), want);
        }
    }

    // Differential test: the prefetching branchless search must return exactly
    // what stdlib slice::binary_search returns for every probe (hit index on a
    // match, None on a miss), on strictly-ascending unique u32 slices — the
    // invariant sort_and_dedup guarantees per block. This is the correctness
    // gate for the prefetch replacement of the inner offsets search.
    #[test]
    fn search_offsets_matches_stdlib_binary_search() {
        // A spread of slice shapes: empty, singletons, dense, sparse, gaps.
        let cases: Vec<Vec<u32>> = vec![
            vec![],
            vec![0],
            vec![7],
            vec![0, 1, 2, 3, 4, 5, 6, 7],
            vec![1, 3, 5, 7, 9, 11],
            vec![0, 100, 200, 5000, 5001, u32::MAX - 1, u32::MAX],
            (0..257u32).collect(),
            (0..1000u32).map(|k| k.wrapping_mul(37)).collect(),
        ];
        for slice in &cases {
            // Probe every element, the neighbors of every element, the ends,
            // and a scattering of interior values -> covers hits + misses.
            let mut probes: Vec<u32> = Vec::new();
            for &v in slice {
                probes.push(v);
                probes.push(v.wrapping_sub(1));
                probes.push(v.wrapping_add(1));
            }
            probes.push(0);
            probes.push(u32::MAX);
            for d in 0..64u32 {
                probes.push(d.wrapping_mul(97));
            }
            for &d in &probes {
                let want = slice.binary_search(&d).ok();
                let got = search_offsets(slice, d);
                assert_eq!(got, want, "slice={slice:?} probe={d}");
            }
        }
    }

    proptest::proptest! {
        // Property form of the differential gate: on ANY sorted unique u32
        // slice, search_offsets agrees with stdlib binary_search for arbitrary
        // probes (members and non-members alike).
        #[test]
        fn prop_search_offsets_matches_stdlib(
            raw in proptest::collection::vec(0u32.., 0..300),
            probes in proptest::collection::vec(0u32.., 0..80),
        ) {
            let mut slice: Vec<u32> = raw;
            slice.sort_unstable();
            slice.dedup();
            for &d in &probes {
                let want = slice.binary_search(&d).ok();
                let got = search_offsets(&slice, d);
                proptest::prop_assert_eq!(got, want);
            }
            // Every member must also be found at its exact index.
            for (i, &v) in slice.iter().enumerate() {
                proptest::prop_assert_eq!(search_offsets(&slice, v), Some(i));
            }
        }
    }

    proptest::proptest! {
        // Random distinct addresses constrained to a single 4 GB span: every
        // member round-trips via index_of/addr_at, and non-members return None,
        // matching a naive sorted-slice binary search. This locks the
        // single-block fast path against the reference on arbitrary inputs.
        #[test]
        fn prop_single_block_roundtrip(
            raw in proptest::collection::vec(0u32.., 0..400),
            probes in proptest::collection::vec(0u32.., 0..40),
        ) {
            let base = 0x2000_0000_0000u64;
            let mut addrs: Vec<u64> = raw.iter().map(|&x| base + x as u64).collect();
            addrs.sort_unstable();
            addrs.dedup();
            let mut m = IdMap::new();
            for &a in &addrs {
                m.push(a);
            }
            m.sort_and_dedup();
            if !addrs.is_empty() {
                proptest::prop_assert_eq!(m.block_base.len(), 1);
            }
            for (i, &a) in addrs.iter().enumerate() {
                proptest::prop_assert_eq!(m.index_of(a), Some(i));
                proptest::prop_assert_eq!(m.addr_at(i), a);
            }
            for &p in &probes {
                let addr = base + p as u64;
                let want = addrs.binary_search(&addr).ok();
                proptest::prop_assert_eq!(m.index_of(addr), want);
            }
        }
    }
}

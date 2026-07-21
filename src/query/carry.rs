//! Compact "carry" codec for cross-phase queries. Phase 1 (pass2) matches
//! objects and stores only what a later phase needs to finish: matched dense
//! indices (delta-varint), optionally with packed scalar columns, or a deduped
//! sorted address frontier. All layouts round-trip byte-exactly and enforce a
//! cap that trips `truncated` so a pathological match set can't blow memory.

use crate::vbyte;

/// Default cap on carried matches. Beyond this the carry stops accepting rows
/// and `truncated()` returns true; the resumed stage then reports a bounded
/// sample rather than OOMing on a query like `SELECT ... FROM java.lang.Object`.
pub const DEFAULT_CARRY_CAP: usize = 1_000_000;

/// How Phase 1 packed the matched objects for a later phase to consume.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CarryLayout {
    /// Just the matched dense indices, delta-varint encoded.
    IndexOnly,
    /// Matched indices plus one packed scalar column per width (bytes-per-value).
    IndexPlusScalars { widths: Vec<u8> },
    /// A deduplicated, sorted set of object addresses (delta-varint u64).
    AddrFrontier,
}

/// Accumulates Phase-1 matches in one of the `CarryLayout`s, enforcing a cap.
#[derive(Debug, Clone)]
pub struct Carry {
    layout: CarryLayout,
    cap: usize,
    truncated: bool,
    idx: Vec<u32>,
    cols: Vec<Vec<u64>>,
    addrs: std::collections::BTreeSet<u64>,
}

impl Carry {
    pub fn index_only(cap: usize) -> Self {
        Self { layout: CarryLayout::IndexOnly, cap, truncated: false,
               idx: Vec::new(), cols: Vec::new(), addrs: std::collections::BTreeSet::new() }
    }
    pub fn layout(&self) -> &CarryLayout { &self.layout }
    pub fn truncated(&self) -> bool { self.truncated }
    pub fn len(&self) -> usize { self.idx.len() }
    pub fn is_empty(&self) -> bool { self.idx.is_empty() }

    /// Record a matched dense index. Silently drops (and sets `truncated`) once
    /// the cap is reached so a runaway match set can't exhaust memory.
    pub fn push_index(&mut self, dense_idx: u32) {
        if self.idx.len() >= self.cap { self.truncated = true; return; }
        self.idx.push(dense_idx);
    }

    /// Decode the carried dense indices, routing through the delta-varint codec
    /// to prove the on-wire form round-trips exactly.
    pub fn indices(&self) -> Vec<u32> {
        let mut buf = Vec::new();
        encode_indices_delta(&self.idx, &mut buf);
        decode_indices_delta(&buf, self.idx.len())
    }

    /// A carry that stores matched indices plus one packed scalar column per entry
    /// in `widths` (bytes-per-value: 1,2,4,8). Values are validated to fit width.
    pub fn index_plus_scalars(cap: usize, widths: Vec<u8>) -> Self {
        let n = widths.len();
        Self {
            layout: CarryLayout::IndexPlusScalars { widths },
            cap, truncated: false, idx: Vec::new(),
            cols: vec![Vec::new(); n],
            addrs: std::collections::BTreeSet::new(),
        }
    }

    /// Record a matched index plus one value per scalar column. Panics if
    /// `vals.len()` mismatches the column count or a value exceeds its byte width
    /// (both are planner/executor invariants, not user input).
    pub fn push_row(&mut self, dense_idx: u32, vals: &[u64]) {
        let CarryLayout::IndexPlusScalars { widths } = &self.layout else {
            panic!("push_row is only valid for IndexPlusScalars, got {:?}", self.layout);
        };
        assert_eq!(vals.len(), widths.len(),
            "push_row expected {} scalar column(s) but got {}", widths.len(), vals.len());
        if self.idx.len() >= self.cap { self.truncated = true; return; }
        for (k, (&v, &w)) in vals.iter().zip(widths.iter()).enumerate() {
            let max = if w >= 8 { u64::MAX } else { (1u64 << (w as u32 * 8)) - 1 };
            assert!(v <= max,
                "scalar column {k}: value {v} does not fit declared width {w} byte(s) (max {max})");
            self.cols[k].push(v);
        }
        self.idx.push(dense_idx);
    }

    /// Decode scalar column `k`, round-tripping through the fixed-width packed form.
    pub fn scalar_column(&self, k: usize) -> Vec<u64> {
        let CarryLayout::IndexPlusScalars { widths } = &self.layout else {
            panic!("scalar_column is only valid for IndexPlusScalars");
        };
        let w = widths[k] as usize;
        let mut buf = Vec::with_capacity(self.cols[k].len() * w);
        for &v in &self.cols[k] { buf.extend_from_slice(&v.to_be_bytes()[8 - w..]); }
        let mut out = Vec::with_capacity(self.cols[k].len());
        for chunk in buf.chunks_exact(w) {
            let mut acc = 0u64;
            for &b in chunk { acc = (acc << 8) | b as u64; }
            out.push(acc);
        }
        out
    }

    /// A carry that accumulates a deduplicated, sorted address frontier. The cap
    /// bounds the number of DISTINCT addresses.
    pub fn addr_frontier(cap: usize) -> Self {
        Self {
            layout: CarryLayout::AddrFrontier, cap, truncated: false,
            idx: Vec::new(), cols: Vec::new(),
            addrs: std::collections::BTreeSet::new(),
        }
    }

    /// Add an address. Duplicates are ignored (never count against the cap); a new
    /// distinct address past the cap sets `truncated`.
    pub fn push_addr(&mut self, addr: u64) {
        if self.addrs.contains(&addr) { return; }
        if self.addrs.len() >= self.cap { self.truncated = true; return; }
        self.addrs.insert(addr);
    }

    /// Decode the sorted, deduplicated frontier, round-tripping through the
    /// delta-varint u64 wire form.
    pub fn addresses(&self) -> Vec<u64> {
        let sorted: Vec<u64> = self.addrs.iter().copied().collect();
        let mut buf = Vec::new();
        vbyte::encode_delta_u64(&sorted, &mut buf);
        vbyte::decode_delta_u64(&buf, sorted.len())
    }
}

/// Delta-varint encode indices in push order. `wrapping_sub` makes even a
/// non-monotonic sequence round-trip exactly.
fn encode_indices_delta(idx: &[u32], out: &mut Vec<u8>) {
    let mut prev = 0u32;
    for &v in idx {
        vbyte::encode(v.wrapping_sub(prev), out);
        prev = v;
    }
}

fn decode_indices_delta(buf: &[u8], count: usize) -> Vec<u32> {
    let mut out = Vec::with_capacity(count);
    let mut prev = 0u32;
    let mut i = 0;
    while i < buf.len() && out.len() < count {
        let (d, n) = vbyte::decode_one(&buf[i..]);
        prev = prev.wrapping_add(d);
        out.push(prev);
        i += n;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_only_roundtrips_sorted_indices() {
        let mut c = Carry::index_only(1_000_000);
        for &i in &[3u32, 3, 10, 4096, 4097, 999_999] {
            c.push_index(i);
        }
        assert!(!c.truncated());
        assert_eq!(c.indices(), vec![3, 3, 10, 4096, 4097, 999_999]);
    }

    #[test]
    fn cap_trips_truncated_and_bounds_len() {
        let mut c = Carry::index_only(2);
        c.push_index(1);
        c.push_index(2);
        assert!(!c.truncated(), "at-cap but not over yet");
        c.push_index(3); // over the cap
        assert!(c.truncated());
        assert_eq!(c.len(), 2, "cap must bound stored indices");
        assert_eq!(c.indices(), vec![1, 2]);
    }

    #[test]
    fn empty_carry_roundtrips_to_empty() {
        let c = Carry::index_only(10);
        assert!(c.is_empty());
        assert!(!c.truncated());
        assert_eq!(c.indices(), Vec::<u32>::new());
    }

    #[test]
    fn descending_pushes_still_roundtrip_exactly() {
        let mut c = Carry::index_only(100);
        for &i in &[100u32, 5, 5, 42, 0] { c.push_index(i); }
        assert_eq!(c.indices(), vec![100, 5, 5, 42, 0]);
    }

    #[test]
    fn index_plus_scalars_roundtrips_columns() {
        let mut c = Carry::index_plus_scalars(1000, vec![1, 4]);
        c.push_row(10, &[7, 0x0001_0000]);
        c.push_row(11, &[255, 42]);
        assert!(!c.truncated());
        assert_eq!(c.indices(), vec![10, 11]);
        assert_eq!(c.scalar_column(0), vec![7, 255]);
        assert_eq!(c.scalar_column(1), vec![0x0001_0000, 42]);
    }

    #[test]
    fn scalar_width_overflow_panics_with_message() {
        let mut c = Carry::index_plus_scalars(10, vec![1]);
        let err = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            c.push_row(0, &[256]); // 256 does not fit in 1 byte
        }));
        assert!(err.is_err(), "over-wide scalar must panic");
    }

    #[test]
    fn addr_frontier_dedups_and_sorts() {
        let mut c = Carry::addr_frontier(100);
        for &a in &[0x5000u64, 0x1000, 0x5000, 0x1000, 0x9abc] { c.push_addr(a); }
        assert_eq!(c.addresses(), vec![0x1000, 0x5000, 0x9abc]);
        assert!(!c.truncated());
    }

    #[test]
    fn addr_frontier_cap_counts_distinct() {
        let mut c = Carry::addr_frontier(2);
        c.push_addr(0x10);
        c.push_addr(0x10); // dup, still 1 distinct
        c.push_addr(0x20); // 2 distinct — at cap
        c.push_addr(0x30); // over the distinct cap
        assert!(c.truncated());
        assert_eq!(c.addresses(), vec![0x10, 0x20]);
    }
}

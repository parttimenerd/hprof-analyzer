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
}

//! Delta+vbyte compressed edge store for retained reference edges.
//!
//! MEMORY-CRITICAL. The forward reference graph is ~7.5 GB and RSS peaks
//! ~22 GB, so any structure that retains a subset of edges must stay compact.
//! Each row's sorted target list is delta-encoded and vbyte-packed into a
//! single shared `blob`; a small index maps `from` rows to their byte slice.
//!
//! # L2 invariant
//! This store NEVER exposes or expands to a flat `Vec<u32>`/`&[u32]` of ALL
//! edges across rows. Decode is strictly per-row, on demand, via
//! [`RetainedEdges::targets_of`]. There is deliberately no "all edges" accessor.
//! Do not add one.

// Nothing consumes this until Task 40/41 wires the edge executor; keep the
// item set alive for now (matches the codebase convention in runflags.rs).
#![allow(dead_code)]

use crate::vbyte;

/// Accumulates rows of `(from, sorted_targets)` into a shared delta+vbyte blob.
///
/// Rows may be pushed in any `from` order; [`finish`](Self::finish) sorts the
/// index so lookups can binary-search. The blob is append-only and never
/// materializes a flat edge list.
pub struct RetainedEdgesBuilder {
    blob: Vec<u8>,
    /// `(from_row, byte_offset, count)`, pushed in call order.
    index: Vec<(u32, u32, u32)>,
}

/// Finished, queryable edge store. Index is sorted by `from` so
/// [`targets_of`](Self::targets_of) can binary-search.
pub struct RetainedEdges {
    blob: Vec<u8>,
    /// `(from_row, byte_offset, count)`, sorted by `from_row` at `finish()`.
    index: Vec<(u32, u32, u32)>,
}

impl RetainedEdgesBuilder {
    /// Create an empty builder.
    pub fn new() -> Self {
        RetainedEdgesBuilder {
            blob: Vec::new(),
            index: Vec::new(),
        }
    }

    /// Append one row.
    ///
    /// # Precondition
    /// `sorted_targets` MUST already be sorted ascending. The caller is
    /// responsible for sorting; this method does not sort (delta encoding
    /// assumes ascending order). A `debug_assert!` guards the invariant in
    /// debug/test builds.
    ///
    /// An empty `sorted_targets` slice is valid and encodes to zero bytes; the
    /// row still appears in the index (and thus in [`RetainedEdges::from_rows`]).
    pub fn push_row(&mut self, from: u32, sorted_targets: &[u32]) {
        debug_assert!(
            sorted_targets.windows(2).all(|w| w[0] <= w[1]),
            "push_row targets must be sorted ascending, got {sorted_targets:?}"
        );
        let offset = self.blob.len() as u32;
        vbyte::encode_delta(sorted_targets, &mut self.blob);
        self.index.push((from, offset, sorted_targets.len() as u32));
    }

    /// Finalize: sort the index by `from` for binary-search lookups and move the
    /// blob over unchanged.
    ///
    /// Rows pushed out of `from` order are reordered here. Duplicate `from`
    /// values are NOT merged — both entries are retained (a stable sort keeps
    /// their push order); the canonical use pushes each `from` exactly once.
    pub fn finish(mut self) -> RetainedEdges {
        self.index.sort_by_key(|&(from, _, _)| from);
        RetainedEdges {
            blob: self.blob,
            index: self.index,
        }
    }
}

impl Default for RetainedEdgesBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl RetainedEdges {
    /// Decode the ascending target list for `from`, or an empty `Vec` if `from`
    /// was never pushed. Decode is per-row: only this row's blob slice is read.
    pub fn targets_of(&self, from: u32) -> Vec<u32> {
        match self.index.binary_search_by_key(&from, |&(f, _, _)| f) {
            Ok(i) => {
                let (_, offset, count) = self.index[i];
                vbyte::decode_delta(&self.blob[offset as usize..], count as usize)
            }
            Err(_) => Vec::new(),
        }
    }

    /// The `from` column of the index, ascending (after `finish`).
    #[allow(clippy::wrong_self_convention)] // `from_rows` names the `from` column, not a constructor
    pub fn from_rows(&self) -> Vec<u32> {
        self.index.iter().map(|&(from, _, _)| from).collect()
    }

    /// Total compressed blob size in bytes. A store containing only empty-target
    /// rows has `compressed_len() == 0` even though `from_rows()` is non-empty.
    pub fn compressed_len(&self) -> usize {
        self.blob.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retained_edges_roundtrip_targets() {
        let mut b = RetainedEdgesBuilder::new();
        b.push_row(0, &[3, 7, 9]);
        b.push_row(5, &[1, 2, 100]);
        let re = b.finish();
        assert_eq!(re.targets_of(0), vec![3, 7, 9]);
        assert_eq!(re.targets_of(5), vec![1, 2, 100]);
        assert_eq!(re.targets_of(1), Vec::<u32>::new());
        assert_eq!(re.from_rows(), vec![0, 5]);
    }

    #[test]
    fn retained_edges_stays_compressed() {
        let mut b = RetainedEdgesBuilder::new();
        for i in 0..1000u32 {
            // small deltas so vbyte packs each into a single byte
            let base = i * 10;
            b.push_row(i, &[base, base + 1, base + 2]);
        }
        let re = b.finish();
        assert!(
            re.compressed_len() < 1000 * 3 * 4,
            "delta+vbyte should beat flat u32: got {}",
            re.compressed_len()
        );
    }

    #[test]
    fn empty_row() {
        let mut b = RetainedEdgesBuilder::new();
        b.push_row(2, &[]);
        let re = b.finish();
        assert_eq!(re.targets_of(2), Vec::<u32>::new());
        assert!(re.from_rows().contains(&2));
    }

    #[test]
    fn single_target_row() {
        let mut b = RetainedEdgesBuilder::new();
        b.push_row(9, &[42]);
        let re = b.finish();
        assert_eq!(re.targets_of(9), vec![42]);
    }

    #[test]
    fn rows_pushed_out_of_order_sorted_at_finish() {
        let mut b = RetainedEdgesBuilder::new();
        b.push_row(5, &[50, 51]);
        b.push_row(1, &[10, 11, 12]);
        b.push_row(3, &[30]);
        let re = b.finish();
        assert_eq!(re.from_rows(), vec![1, 3, 5]);
        assert_eq!(re.targets_of(1), vec![10, 11, 12]);
        assert_eq!(re.targets_of(3), vec![30]);
        assert_eq!(re.targets_of(5), vec![50, 51]);
    }

    #[test]
    fn large_target_values() {
        let mut b = RetainedEdgesBuilder::new();
        let targets = [1u32, 1000, u32::MAX - 1];
        b.push_row(7, &targets);
        let re = b.finish();
        assert_eq!(re.targets_of(7), targets.to_vec());
    }

    #[test]
    fn absent_from_returns_empty() {
        let mut b = RetainedEdgesBuilder::new();
        b.push_row(0, &[1, 2, 3]);
        b.push_row(10, &[4, 5]);
        let re = b.finish();
        assert_eq!(re.targets_of(999), Vec::<u32>::new());
        // Also absent between existing keys (binary search miss in the middle).
        assert_eq!(re.targets_of(5), Vec::<u32>::new());
    }

    #[test]
    fn compressed_len_is_blob_len() {
        // Non-empty rows produce a positive, stable blob length.
        let mut b = RetainedEdgesBuilder::new();
        b.push_row(0, &[1, 2, 3]);
        b.push_row(1, &[4, 5, 6]);
        let re = b.finish();
        assert!(re.compressed_len() > 0);

        // A store with ONLY empty-target rows encodes zero bytes, yet the rows
        // still appear in the index — documents the empty-row edge case.
        let mut e = RetainedEdgesBuilder::new();
        e.push_row(0, &[]);
        e.push_row(1, &[]);
        let empty = e.finish();
        assert_eq!(empty.compressed_len(), 0);
        assert_eq!(empty.from_rows(), vec![0, 1]);
    }

    #[test]
    fn empty_builder_has_no_rows() {
        let re = RetainedEdgesBuilder::new().finish();
        assert_eq!(re.from_rows(), Vec::<u32>::new());
        assert_eq!(re.compressed_len(), 0);
        assert_eq!(re.targets_of(0), Vec::<u32>::new());
    }

    #[test]
    fn multi_row_offset_integrity() {
        // Interleave rows of varying widths to prove each row's byte offset is
        // recorded independently and decode reads exactly `count` values.
        let mut b = RetainedEdgesBuilder::new();
        b.push_row(0, &[1]);
        b.push_row(1, &[100, 200, 300, 400]);
        b.push_row(2, &[]);
        b.push_row(3, &[7, 8]);
        let re = b.finish();
        assert_eq!(re.targets_of(0), vec![1]);
        assert_eq!(re.targets_of(1), vec![100, 200, 300, 400]);
        assert_eq!(re.targets_of(2), Vec::<u32>::new());
        assert_eq!(re.targets_of(3), vec![7, 8]);
    }

    #[test]
    fn duplicate_from_rows_both_retained() {
        // Duplicate `from` is not merged; both index entries survive finish().
        // (binary_search returns *some* matching entry; we assert the count of
        // matching from_rows rather than which target list wins.)
        let mut b = RetainedEdgesBuilder::new();
        b.push_row(4, &[1, 2]);
        b.push_row(4, &[9]);
        let re = b.finish();
        let fours = re.from_rows().iter().filter(|&&f| f == 4).count();
        assert_eq!(fours, 2, "duplicate from rows must both be retained");
    }
}

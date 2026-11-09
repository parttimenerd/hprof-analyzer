//! Query-gated `java.lang.String` decode side table.
//!
//! When at least one query requests `toString(s)`, the pass2 scan captures
//! `(dense_idx → (backing_array_addr, coder))` into a `StringCapture` table.
//! After the scan, one `scan_prim_arrays` pass decodes each distinct backing
//! array and builds the final `HashMap<u32, String>` (dense index → text).
//! The production path is **entirely query-gated**: a non-toString run never
//! allocates or populates any of these structures.

use std::collections::{HashMap, HashSet};

/// Cap on the number of String instances captured during the scan. Matches the
/// general RefWalk cap to keep RSS bounded on pathological dumps. Once hit,
/// further captures are silently dropped and `truncated` is set.
pub const STRING_VALUES_CAP: usize = 500_000;

/// Scan-time capture: maps `dense_idx → (backing_array_addr, coder)`.
/// Armed only when at least one query has `needs.string_values`.
pub struct StringCapture {
    /// dense_idx → (backing_array_addr, coder)
    inner: HashMap<u32, (u64, u8)>,
    /// dense_idx values that are empty strings (arr_addr == 0 in the heap)
    empty: HashSet<u32>,
    cap: usize,
    pub truncated: bool,
}

impl StringCapture {
    pub fn new(cap: usize) -> Self {
        Self {
            inner: HashMap::new(),
            empty: HashSet::new(),
            cap,
            truncated: false,
        }
    }

    /// Record the backing array address and coder for a String instance.
    /// Last write wins (but each dense_idx is visited at most once in practice).
    pub fn insert(&mut self, dense_idx: u32, arr_addr: u64, coder: u8) {
        if self.inner.len() >= self.cap && !self.inner.contains_key(&dense_idx) {
            self.truncated = true;
            return;
        }
        self.inner.insert(dense_idx, (arr_addr, coder));
    }

    /// Record a String with a null backing array (Java `""`) as an empty string.
    pub fn insert_empty(&mut self, dense_idx: u32) {
        self.empty.insert(dense_idx);
    }

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty() && self.empty.is_empty()
    }

    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.inner.len() + self.empty.len()
    }

    /// Decode all captured String instances using one `scan_prim_arrays` pass.
    /// Returns `(dense_idx → String)` map. After this call `self` is consumed.
    pub fn decode_all<O>(self, open: O, id_size: u8) -> std::io::Result<HashMap<u32, String>>
    where
        O: Fn() -> std::io::Result<crate::reader::HprofReader>,
    {
        // Pre-populate empty strings (Java "" — null backing array).
        let mut out = HashMap::with_capacity(self.inner.len() + self.empty.len());
        for dense_idx in self.empty {
            out.insert(dense_idx, String::new());
        }

        if self.inner.is_empty() {
            return Ok(out);
        }

        // Build the wanted-array set and a reverse map: arr_addr → coder.
        let mut arr_coder: HashMap<u64, u8> = HashMap::new();
        for &(arr_addr, coder) in self.inner.values() {
            // STR-01: warn when two Strings share a backing array with conflicting coders.
            let existing = arr_coder.entry(arr_addr).or_insert(coder);
            if *existing != coder {
                eprintln!(
                    "stringvals: conflicting coders ({} vs {}) for backing array 0x{:x}; using first",
                    *existing, coder, arr_addr
                );
            }
        }
        let wanted: HashSet<u64> = arr_coder.keys().copied().collect();

        // One scan_prim_arrays pass decodes each distinct array once.
        let mut arr_text: HashMap<u64, String> = HashMap::new();
        crate::pass2::scan_prim_arrays(&open, id_size, &wanted, |addr, bytes| {
            if let Some(&coder) = arr_coder.get(&addr) {
                let s = crate::pass2::decode_java_string(bytes, coder);
                arr_text.insert(addr, s);
            }
        })?;

        // Join dense_idx → arr_addr → String.
        for (dense_idx, (arr_addr, _coder)) in self.inner {
            if let Some(s) = arr_text.get(&arr_addr) {
                out.insert(dense_idx, s.clone());
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ============================================================
    // StringCapture cap / truncation tests (mirrors RefWalkEdges
    // cap-test style in refwalk.rs): verify the 500,000 cap and
    // the `truncated` flag behave exactly as documented.
    // ============================================================

    /// Below-cap: inserting fewer entries than the cap leaves all
    /// entries in the map and `truncated` stays false.
    #[test]
    fn string_capture_below_cap_no_truncation() {
        let cap = 10;
        let mut sc = StringCapture::new(cap);
        for i in 0..cap {
            sc.insert(i as u32, i as u64 * 2, 0);
        }
        assert_eq!(sc.len(), cap, "all {cap} entries must be retained");
        assert!(!sc.truncated, "truncated must be false below cap");
    }

    /// Exactly at cap: inserting exactly `cap` entries fills the map
    /// without setting `truncated`.
    #[test]
    fn string_capture_at_cap_not_truncated() {
        let cap = 5;
        let mut sc = StringCapture::new(cap);
        for i in 0..cap {
            sc.insert(i as u32, i as u64, 1);
        }
        assert_eq!(sc.len(), cap);
        assert!(
            !sc.truncated,
            "truncated must not be true when exactly at cap"
        );
    }

    /// Past cap: once the cap is exceeded the `truncated` flag becomes
    /// true and the map does NOT grow beyond the cap.
    #[test]
    fn string_capture_past_cap_truncated_and_size_bounded() {
        let cap = 8;
        let mut sc = StringCapture::new(cap);
        // Insert cap+1 entries (one beyond the limit).
        for i in 0..=(cap as u32) {
            sc.insert(i, i as u64, 0);
        }
        assert!(
            sc.truncated,
            "truncated must be true after inserting past the cap"
        );
        assert_eq!(
            sc.len(),
            cap,
            "map must not grow beyond cap; got {}",
            sc.len()
        );
    }

    /// Inserting many entries (well past the cap) still caps the map
    /// at exactly `cap` and keeps `truncated == true`.
    #[test]
    fn string_capture_many_past_cap_stays_bounded() {
        let cap = 4;
        let mut sc = StringCapture::new(cap);
        for i in 0..(cap * 3) {
            sc.insert(i as u32, i as u64, 0);
        }
        assert!(sc.truncated, "truncated after many insertions");
        assert!(
            sc.len() <= cap,
            "len {} must not exceed cap {}",
            sc.len(),
            cap
        );
    }

    /// Updating an existing key (same dense_idx) does NOT count toward
    /// the cap check (the cap counts DISTINCT entries, not writes).
    #[test]
    fn string_capture_update_existing_key_does_not_count_twice() {
        let cap = 3;
        let mut sc = StringCapture::new(cap);
        sc.insert(0, 100, 0);
        sc.insert(1, 200, 0);
        // Overwrite key 0 — this should NOT trigger truncation.
        sc.insert(0, 999, 1);
        assert!(
            !sc.truncated,
            "overwriting an existing key must not trigger truncation"
        );
        assert_eq!(sc.len(), 2, "still only 2 distinct keys");
    }

    /// Empty capture: `is_empty()` returns true and `truncated` is false.
    #[test]
    fn string_capture_empty_state() {
        let sc = StringCapture::new(STRING_VALUES_CAP);
        assert!(sc.is_empty());
        assert!(!sc.truncated);
        assert_eq!(sc.len(), 0);
    }

    /// The production cap constant `STRING_VALUES_CAP` is 500,000.
    /// Pin this value so a refactor cannot silently shrink or grow it.
    #[test]
    fn string_values_cap_constant_is_500k() {
        assert_eq!(STRING_VALUES_CAP, 500_000);
    }
}

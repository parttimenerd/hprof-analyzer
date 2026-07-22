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
    cap: usize,
    pub truncated: bool,
}

impl StringCapture {
    pub fn new(cap: usize) -> Self {
        Self {
            inner: HashMap::new(),
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

    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// Decode all captured String instances using one `scan_prim_arrays` pass.
    /// Returns `(dense_idx → String)` map. After this call `self` is consumed.
    pub fn decode_all(self, path: &str, id_size: u8) -> std::io::Result<HashMap<u32, String>> {
        if self.inner.is_empty() {
            return Ok(HashMap::new());
        }

        // Build the wanted-array set and a reverse map: arr_addr → coder.
        let mut arr_coder: HashMap<u64, u8> = HashMap::new();
        for &(arr_addr, coder) in self.inner.values() {
            arr_coder.entry(arr_addr).or_insert(coder);
        }
        let wanted: HashSet<u64> = arr_coder.keys().copied().collect();

        // One scan_prim_arrays pass decodes each distinct array once.
        let mut arr_text: HashMap<u64, String> = HashMap::new();
        crate::pass2::scan_prim_arrays(path, id_size, &wanted, |addr, bytes| {
            if let Some(&coder) = arr_coder.get(&addr) {
                let s = crate::pass2::decode_java_string(bytes, coder);
                arr_text.insert(addr, s);
            }
        })?;

        // Join dense_idx → arr_addr → String.
        let mut out = HashMap::with_capacity(self.inner.len());
        for (dense_idx, (arr_addr, _coder)) in self.inner {
            if let Some(s) = arr_text.get(&arr_addr) {
                out.insert(dense_idx, s.clone());
            }
        }
        Ok(out)
    }
}

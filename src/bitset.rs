/// Compact 1-bit-per-element set backed by `Vec<u64>` (n/64 words).
///
/// Replaces `Vec<bool>` for the hasSameClassAncestor flag: ~64 MB vs ~0.51 GB
/// at n=514M. `get`/`set` are branch-light bit ops; behaviour is identical to
/// the old `has_same[i]` bool reads/writes.
#[derive(Clone, Default)]
pub struct Bitset {
    words: Vec<u64>,
}

impl Bitset {
    pub fn with_len(n: usize) -> Self {
        Bitset {
            words: vec![0u64; n.div_ceil(64)],
        }
    }

    #[inline]
    pub fn set(&mut self, i: usize) {
        self.words[i >> 6] |= 1u64 << (i & 63);
    }

    #[inline]
    pub fn get(&self, i: usize) -> bool {
        (self.words[i >> 6] >> (i & 63)) & 1 != 0
    }
}

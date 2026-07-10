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

//! Chunked u32 store for large fill-then-consume arrays.
//!
//! `inb_flat` in the inbound CSR build is filled by random-access scatter, then
//! consumed strictly left-to-right in Phase-4. A single flat `Vec<u32>` must
//! stay fully live until the last node is encoded, so it coexists with the
//! fully-built `inb_data` at the global RSS peak. Splitting the backing store
//! into fixed power-of-two chunks lets Phase-4 free each chunk the moment its
//! read cursor passes it, so remaining(inb_flat)+built(inb_data) peaks far below
//! their sum.
//!
//! Indexing uses shift/mask (CHUNK_LOG) so the scatter-fill hot path stays cheap.

const CHUNK_LOG: usize = 26; // 2^26 u32 = 64M slots = 256 MB per chunk
const CHUNK_LEN: usize = 1 << CHUNK_LOG;
const CHUNK_MASK: usize = CHUNK_LEN - 1;

pub struct ChunkU32 {
    chunks: Vec<Vec<u32>>,
    len: usize,
}

impl ChunkU32 {
    /// Allocate `len` u32 slots, zero-initialized, across power-of-two chunks.
    pub fn zeroed(len: usize) -> Self {
        let nchunks = len.div_ceil(CHUNK_LEN);
        let mut chunks = Vec::with_capacity(nchunks);
        let mut remaining = len;
        for _ in 0..nchunks {
            let this = remaining.min(CHUNK_LEN);
            chunks.push(vec![0u32; this]);
            remaining -= this;
        }
        ChunkU32 { chunks, len }
    }

    #[inline(always)]
    pub fn set(&mut self, idx: usize, val: u32) {
        let c = idx >> CHUNK_LOG;
        let o = idx & CHUNK_MASK;
        self.chunks[c][o] = val;
    }

    #[inline(always)]
    pub fn get(&self, idx: usize) -> u32 {
        self.chunks[idx >> CHUNK_LOG][idx & CHUNK_MASK]
    }

    pub fn len(&self) -> usize {
        self.len
    }

    /// Free every chunk whose slots are entirely below `boundary` (exclusive).
    /// Idempotent: already-freed chunks stay empty. Call as the Phase-4 read
    /// cursor advances so consumed backing memory is returned promptly.
    pub fn free_below(&mut self, boundary: usize) {
        let last_chunk = boundary >> CHUNK_LOG; // chunks strictly before this are fully consumed
        for c in 0..last_chunk {
            if !self.chunks[c].is_empty() {
                self.chunks[c] = Vec::new();
            }
        }
    }

    /// Copy the slots [start, end) into `out` (cleared first). The range may
    /// straddle a chunk boundary; both source chunks must still be live.
    pub fn copy_range(&self, start: usize, end: usize, out: &mut Vec<u32>) {
        out.clear();
        let mut i = start;
        while i < end {
            let c = i >> CHUNK_LOG;
            let o = i & CHUNK_MASK;
            let chunk = &self.chunks[c];
            let take = (CHUNK_LEN - o).min(end - i);
            out.extend_from_slice(&chunk[o..o + take]);
            i += take;
        }
    }
}

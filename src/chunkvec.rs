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
//!
//! Each chunk is backed by an anonymous mmap on Linux so that when it is freed
//! (via `free_below` or drop) the pages are immediately returned to the OS via
//! `munmap`, bypassing glibc's free-list. Without this, glibc retains freed
//! pages in its arena and they are faulted back in on the next large malloc,
//! inflating peak RSS by ~2 GB on the vscode dump.

const CHUNK_LOG: usize = 26; // 2^26 u32 = 64M slots = 256 MB per chunk
const CHUNK_LEN: usize = 1 << CHUNK_LOG;
const CHUNK_MASK: usize = CHUNK_LEN - 1;

// Minimum chunk size (in bytes) to back with mmap rather than the heap.
// 1 MB is well above glibc's MMAP_THRESHOLD (128 KB) default but we want
// every non-trivial chunk to be mmap-backed so drops always return pages.
#[cfg(target_os = "linux")]
const MMAP_MIN_BYTES: usize = 1 << 20; // 1 MB

/// A single contiguous u32 slab, either heap-backed (Vec<u32>) or mmap-backed.
/// The mmap variant bypasses glibc's free-list so pages are returned to the OS
/// immediately on drop, preventing the dirty-page faultback that inflates RSS.
enum Chunk {
    Heap(Vec<u32>),
    #[cfg(target_os = "linux")]
    Mmap {
        ptr: *mut u32,
        len: usize, // element count
    },
}

#[cfg(target_os = "linux")]
unsafe impl Send for Chunk {}
#[cfg(target_os = "linux")]
unsafe impl Sync for Chunk {}

impl Default for Chunk {
    fn default() -> Self {
        Chunk::Heap(Vec::new())
    }
}

impl Chunk {
    fn new_zeroed(len: usize) -> Self {
        #[cfg(target_os = "linux")]
        let bytes = len * std::mem::size_of::<u32>();
        #[cfg(target_os = "linux")]
        if bytes >= MMAP_MIN_BYTES {
            let ptr = unsafe {
                libc::mmap(
                    std::ptr::null_mut(),
                    bytes,
                    libc::PROT_READ | libc::PROT_WRITE,
                    libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                    -1,
                    0,
                ) as *mut u32
            };
            if ptr != libc::MAP_FAILED as *mut u32 {
                // mmap returns zeroed pages — no explicit write needed.
                return Chunk::Mmap { ptr, len };
            }
            // Fall through to heap on mmap failure.
        }
        Chunk::Heap(vec![0u32; len])
    }

    #[inline(always)]
    fn get(&self, off: usize) -> u32 {
        match self {
            Chunk::Heap(v) => v[off],
            #[cfg(target_os = "linux")]
            Chunk::Mmap { ptr, .. } => unsafe { *ptr.add(off) },
        }
    }

    #[inline(always)]
    fn set(&mut self, off: usize, val: u32) {
        match self {
            Chunk::Heap(v) => v[off] = val,
            #[cfg(target_os = "linux")]
            Chunk::Mmap { ptr, .. } => unsafe { *ptr.add(off) = val },
        }
    }

    fn as_slice(&self, off: usize, end: usize) -> &[u32] {
        match self {
            Chunk::Heap(v) => &v[off..end],
            #[cfg(target_os = "linux")]
            Chunk::Mmap { ptr, .. } => unsafe {
                std::slice::from_raw_parts(ptr.add(off), end - off)
            },
        }
    }

    #[allow(dead_code)]
    fn as_ptr(&self) -> *const u32 {
        match self {
            Chunk::Heap(v) => v.as_ptr(),
            #[cfg(target_os = "linux")]
            Chunk::Mmap { ptr, .. } => *ptr as *const u32,
        }
    }

    fn is_empty(&self) -> bool {
        match self {
            Chunk::Heap(v) => v.is_empty(),
            #[cfg(target_os = "linux")]
            Chunk::Mmap { len, .. } => *len == 0,
        }
    }

    #[allow(dead_code)]
    fn elem_len(&self) -> usize {
        match self {
            Chunk::Heap(v) => v.len(),
            #[cfg(target_os = "linux")]
            Chunk::Mmap { len, .. } => *len,
        }
    }

    /// Release this chunk's backing memory to the OS immediately.
    fn free(&mut self) {
        match self {
            Chunk::Heap(v) => {
                *v = Vec::new();
            }
            #[cfg(target_os = "linux")]
            Chunk::Mmap { ptr, len } => {
                if *len > 0 {
                    let bytes = *len * std::mem::size_of::<u32>();
                    unsafe {
                        libc::munmap(*ptr as *mut libc::c_void, bytes);
                    }
                    *len = 0;
                }
            }
        }
    }
}

impl Drop for Chunk {
    fn drop(&mut self) {
        self.free();
    }
}

/// Fill-then-consume u32 array split into fixed 256 MB chunks so each chunk can
/// be freed the instant its read cursor passes it (see module docs for the RSS
/// rationale). Empty inner chunks mark already-freed slots.
#[derive(Default)]
pub struct ChunkU32 {
    chunks: Vec<Chunk>,
}

impl ChunkU32 {
    /// Returns true if no slots are allocated (len == 0).
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.chunks.is_empty()
    }

    /// Build a ChunkU32 from a flat Vec (useful in tests).
    #[cfg(test)]
    pub fn from_vec(v: Vec<u32>) -> Self {
        let mut c = Self::zeroed(v.len());
        for (i, &x) in v.iter().enumerate() {
            c.set(i, x);
        }
        c
    }

    /// Allocate `len` u32 slots, zero-initialized, across power-of-two chunks.
    /// On Linux each chunk is backed by an anonymous mmap so that freeing it
    /// immediately returns pages to the OS (bypassing glibc's free-list).
    pub fn zeroed(len: usize) -> Self {
        let nchunks = len.div_ceil(CHUNK_LEN);
        let mut chunks = Vec::with_capacity(nchunks);
        let mut remaining = len;
        for _ in 0..nchunks {
            let this = remaining.min(CHUNK_LEN);
            chunks.push(Chunk::new_zeroed(this));
            remaining -= this;
        }
        ChunkU32 { chunks }
    }

    /// Store `val` at `idx` (shift/mask chunk lookup; hot scatter-fill path).
    #[inline(always)]
    pub fn set(&mut self, idx: usize, val: u32) {
        let c = idx >> CHUNK_LOG;
        let o = idx & CHUNK_MASK;
        self.chunks[c].set(o, val);
    }

    /// Get the value at `idx`.
    #[inline(always)]
    pub fn get(&self, idx: usize) -> u32 {
        let c = idx >> CHUNK_LOG;
        let o = idx & CHUNK_MASK;
        self.chunks[c].get(o)
    }

    /// Free every chunk whose slots are entirely below `boundary` (exclusive).
    /// Idempotent: already-freed chunks stay empty. Call as the Phase-4 read
    /// cursor advances so consumed backing memory is returned promptly.
    pub fn free_below(&mut self, boundary: usize) {
        let last_chunk = boundary >> CHUNK_LOG; // chunks strictly before this are fully consumed
        for c in 0..last_chunk {
            if !self.chunks[c].is_empty() {
                // For Mmap chunks, free() calls munmap which immediately returns
                // pages to the OS. For Heap chunks, MADV_DONTNEED the pages first
                // (so glibc returns them) then free the Vec.
                #[cfg(target_os = "linux")]
                if let Chunk::Heap(ref v) = self.chunks[c] {
                    let ptr = v.as_ptr() as *mut libc::c_void;
                    let len = v.len() * std::mem::size_of::<u32>();
                    unsafe {
                        libc::madvise(ptr, len, libc::MADV_DONTNEED);
                    }
                }
                self.chunks[c].free();
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
            let chunk_end = ((c + 1) << CHUNK_LOG).min(end);
            let take = chunk_end - i;
            out.extend_from_slice(self.chunks[c].as_slice(o, o + take));
            i += take;
        }
    }

    /// Return a direct slice for `[start, end)` when the range lies entirely
    /// within a single chunk. Returns `None` when the range straddles a boundary
    /// (caller must fall back to `get` or `copy_range`). Zero-cost for the
    /// common case where a node's adjacency list fits inside one 256 MB chunk.
    #[inline(always)]
    pub fn range_slice(&self, start: usize, end: usize) -> Option<&[u32]> {
        if start >= end {
            return Some(&[]);
        }
        let c0 = start >> CHUNK_LOG;
        let c1 = (end - 1) >> CHUNK_LOG;
        if c0 == c1 {
            let o0 = start & CHUNK_MASK;
            let o1 = end & CHUNK_MASK;
            // end is exclusive; if end falls exactly on a chunk boundary, o1==0
            // which means the slice ends at the chunk's last element.
            let end_off = if o1 == 0 { CHUNK_LEN } else { o1 };
            Some(self.chunks[c0].as_slice(o0, end_off))
        } else {
            None
        }
    }
}

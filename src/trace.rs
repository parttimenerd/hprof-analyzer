//! Lightweight RSS tracing. Reads /proc/self/statm and prints resident MB at
//! labeled points. Gated behind a process-global flag set from `--trace-rss`
//! so production runs stay silent and pay no cost beyond an atomic load.

use std::sync::atomic::{AtomicBool, Ordering};

static TRACE: AtomicBool = AtomicBool::new(false);

pub fn set_enabled(on: bool) {
    TRACE.store(on, Ordering::Relaxed);
}

pub fn enabled() -> bool {
    TRACE.load(Ordering::Relaxed)
}

/// Resident set size in MB from /proc/self/statm (field 2 = resident pages).
fn rss_mb() -> u64 {
    match std::fs::read_to_string("/proc/self/statm") {
        Ok(s) => {
            let resident_pages: u64 = s
                .split_whitespace()
                .nth(1)
                .and_then(|f| f.parse().ok())
                .unwrap_or(0);
            resident_pages * 4096 / (1024 * 1024)
        }
        Err(_) => 0,
    }
}

/// Print `label RSS=NNNN MB` to stderr if tracing is enabled.
pub fn probe(label: &str) {
    if enabled() {
        eprintln!("[trace-rss] {label} RSS={} MB", rss_mb());
    }
}

// glibc malloc_trim: return free memory from the top of the heap to the OS.
// Declared directly (no libc crate). No-op / harmless on non-glibc allocators.
unsafe extern "C" {
    fn malloc_trim(pad: usize) -> i32;
}

/// Ask the allocator to return freed pages to the OS. Called after large Vecs
/// are dropped at stage boundaries so freed arenas do not inflate peak RSS
/// (glibc otherwise retains freed pages, pushing the high-water mark ~3-4 GB
/// above the genuinely-live set). Safe: malloc_trim only releases already-free
/// memory. Gated to run always (cheap: one syscall-ish call per stage).
pub fn trim() {
    unsafe {
        malloc_trim(0);
    }
}

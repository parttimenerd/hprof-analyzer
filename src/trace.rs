//! Lightweight RSS tracing. Reads /proc/self/statm and prints resident MB at
//! labeled points. Gated behind a process-global flag set from `--trace-rss`
//! so production runs stay silent and pay no cost beyond an atomic load.

use std::sync::atomic::{AtomicBool, Ordering};

static TRACE: AtomicBool = AtomicBool::new(false);

/// Enable or disable RSS tracing process-wide (set once from `--trace-rss`).
pub fn set_enabled(on: bool) {
    TRACE.store(on, Ordering::Relaxed);
}

/// Whether RSS tracing is currently on (cheap atomic load on the hot path).
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

/// Peak resident set (VmHWM) in MB from /proc/self/status. Monotonic kernel
/// high-water mark — matches `/usr/bin/time -v` "Maximum resident set size".
/// Its INCREASE between two probes attributes the true peak to a phase, which
/// the current-RSS probe and a coarse /proc sampler both miss for short spikes.
fn peak_mb() -> u64 {
    match std::fs::read_to_string("/proc/self/status") {
        Ok(s) => s
            .lines()
            .find_map(|l| l.strip_prefix("VmHWM:"))
            .and_then(|v| v.split_whitespace().next())
            .and_then(|kb| kb.parse::<u64>().ok())
            .map(|kb| kb / 1024)
            .unwrap_or(0),
        Err(_) => 0,
    }
}

/// Print `label RSS=NNNN MB (peak NNNN)` to stderr if tracing is enabled.
pub fn probe(label: &str) {
    if enabled() {
        eprintln!(
            "[trace-rss] {label} RSS={} MB (peak {} MB)",
            rss_mb(),
            peak_mb()
        );
    }
}

// glibc malloc_trim: return free memory from the top of the heap to the OS.
// Declared directly (no libc crate). glibc-only; absent on macOS/BSD libc.
#[cfg(target_os = "linux")]
unsafe extern "C" {
    fn malloc_trim(pad: usize) -> i32;
}

/// Ask the allocator to return freed pages to the OS. Called after large Vecs
/// are dropped at stage boundaries so freed arenas do not inflate peak RSS
/// (glibc otherwise retains freed pages, pushing the high-water mark ~3-4 GB
/// above the genuinely-live set). Safe: malloc_trim only releases already-free
/// memory. Gated to run always (cheap: one syscall-ish call per stage).
/// Non-Linux (macOS dev builds) has no glibc malloc_trim, so this is a no-op there.
pub fn trim() {
    #[cfg(target_os = "linux")]
    unsafe {
        malloc_trim(0);
    }
}

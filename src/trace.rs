//! Lightweight RSS tracing. Reads /proc/self/statm and prints resident MB at
//! labeled points. Gated behind a process-global flag set from `--trace-rss`
//! so production runs stay silent and pay no cost beyond an atomic load.

#![allow(dead_code)]

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

/// Resident set size in MB. Linux reads /proc/self/statm; macOS uses getrusage.
fn rss_mb() -> u64 {
    #[cfg(target_os = "linux")]
    {
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
    #[cfg(all(not(target_os = "linux"), unix))]
    {
        // getrusage RUSAGE_SELF: ru_maxrss is bytes on macOS, KB on Linux.
        let mut ru = unsafe { std::mem::zeroed::<libc::rusage>() };
        if unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut ru) } == 0 {
            // macOS ru_maxrss is bytes
            ru.ru_maxrss as u64 / (1024 * 1024)
        } else {
            0
        }
    }
    #[cfg(not(any(target_os = "linux", unix)))]
    {
        0
    }
}

/// Peak resident set in MB. On Linux uses VmHWM from /proc/self/status.
/// On macOS getrusage already returns the high-water mark (ru_maxrss).
fn peak_mb() -> u64 {
    #[cfg(target_os = "linux")]
    {
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
    #[cfg(all(not(target_os = "linux"), unix))]
    {
        rss_mb() // on macOS ru_maxrss is already the peak HWM
    }
    #[cfg(not(any(target_os = "linux", unix)))]
    {
        0
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
// Declared directly (no libc crate). glibc-only: absent on macOS/BSD libc AND
// on musl (static-musl builds link against musl, which has no malloc_trim), so
// the guard is target_env = "gnu", not just target_os = "linux".
#[cfg(all(target_os = "linux", target_env = "gnu"))]
unsafe extern "C" {
    fn malloc_trim(pad: usize) -> i32;
}

/// Ask the allocator to return freed pages to the OS. Called after large Vecs
/// are dropped at stage boundaries so freed arenas do not inflate peak RSS
/// (glibc otherwise retains freed pages, pushing the high-water mark ~3-4 GB
/// above the genuinely-live set). Safe: malloc_trim only releases already-free
/// memory. Gated to run always (cheap: one syscall-ish call per stage).
/// No-op where glibc malloc_trim is unavailable (macOS dev builds, musl).
pub fn trim() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    unsafe {
        malloc_trim(0);
    }
}

/// Drop a large `Vec` and hint the OS to reclaim its pages via MADV_FREE.
/// MADV_FREE (Linux 4.5+) marks the pages as reclaimable under memory pressure
/// without immediately decommitting them, so glibc's allocator can still read
/// its own free-list metadata written during drop(v). MADV_DONTNEED was used
/// previously but it forces an immediate zero-fill on next access, which
/// corrupts glibc's heap bookkeeping (double-free / corruption crash). No-op on
/// non-Linux and on kernels predating MADV_FREE.
pub fn drop_vec<T>(v: Vec<T>) {
    #[cfg(target_os = "linux")]
    {
        let ptr = v.as_ptr() as *mut libc::c_void;
        let bytes = v.capacity() * std::mem::size_of::<T>();
        drop(v);
        if bytes >= 1 << 20 {
            unsafe {
                // MADV_FREE = 8. Use the raw constant to avoid a libc version
                // dependency; it has been stable since Linux 4.5 (2016).
                libc::madvise(ptr, bytes, 8 /* MADV_FREE */);
            }
        }
        return;
    }
    #[allow(unreachable_code)]
    drop(v);
}

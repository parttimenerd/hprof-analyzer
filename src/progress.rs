//! Lightweight, TTY-gated progress reporting for the `analyze` pipeline.
//!
//! Unlike `--verbose` (per-phase timing) and `--trace-rss` (RSS probes), this
//! prints a single rewriting status line to stderr so a long run on a multi-GB
//! dump is not silent. It carries no data structures and does not touch the
//! pipeline's memory discipline: each `phase()` call writes one short string.
//!
//! Enablement is decided once by `main` (see `--progress auto|always|never`)
//! and stored in a process-global flag; `auto` enables only when stderr is a
//! real terminal and neither `--verbose` nor `--trace-rss` is active (those
//! already emit their own per-phase lines, so we would double-report).

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};

static ENABLED: AtomicBool = AtomicBool::new(false);

/// Enable or disable progress output for the rest of the process.
pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
}

fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// Overwrite the current status line with `label`. No-op unless enabled.
/// Uses a carriage return + clear-to-end so successive phases reuse one line.
pub fn phase(label: &str) {
    if !enabled() {
        return;
    }
    // \r returns to column 0; \x1b[K clears to end of line so a shorter label
    // does not leave stale characters from a longer previous one.
    let mut err = std::io::stderr().lock();
    let _ = write!(err, "\r\x1b[K… {label}");
    let _ = err.flush();
}

/// Clear the status line at the end of a run. No-op unless enabled.
pub fn done() {
    if !enabled() {
        return;
    }
    let mut err = std::io::stderr().lock();
    let _ = write!(err, "\r\x1b[K");
    let _ = err.flush();
}

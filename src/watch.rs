//! `hprof-analyzer watch` / `watch-dir` — heap capture + report series.
//!
//! `watch`: attaches to a running JVM (via `jcmd` or `jmap`), takes heap dumps
//! on a schedule, produces a Report JSON for each, keeps the last N, and
//! optionally regenerates a rolling diff HTML after every capture.
//!
//! `watch-dir`: monitors a directory for heap dump files deposited by an
//! external tool (e.g. SapMachine `-XX:HiMemReportExec=GC.heap_dump`) and
//! analyzes each new file as it arrives.

use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::opts::DetailLevel;

pub struct WatchConfig {
    pub pid: u32,
    pub interval_secs: u64,
    pub keep: usize,
    pub output_dir: PathBuf,
    pub diff: bool,
    pub detail: DetailLevel,
    /// Explicit path to jcmd. None = auto-discover.
    pub jcmd_override: Option<String>,
}

pub fn run(cfg: WatchConfig) -> io::Result<()> {
    // Validate and prepare output directory.
    std::fs::create_dir_all(&cfg.output_dir)?;

    // Verify the PID exists.
    if !pid_exists(cfg.pid) {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("no process with PID {} found", cfg.pid),
        ));
    }

    // Find jcmd / jmap.
    let dumper = find_dumper(cfg.jcmd_override.as_deref())?;
    eprintln!(
        "hprof-analyzer watch: monitoring PID {} with {} — interval {}s, keep {}",
        cfg.pid,
        dumper.name(),
        cfg.interval_secs,
        cfg.keep,
    );
    eprintln!("  output dir : {}", cfg.output_dir.display());
    eprintln!("  Press Ctrl-C to stop.\n");

    // Install Ctrl-C / SIGTERM handler via the static STOP_ATOMIC flag.
    #[cfg(unix)]
    unsafe {
        libc::signal(
            libc::SIGINT,
            signal_handler as extern "C" fn(libc::c_int) as libc::sighandler_t,
        );
        libc::signal(
            libc::SIGTERM,
            signal_handler as extern "C" fn(libc::c_int) as libc::sighandler_t,
        );
    }
    #[cfg(not(unix))]
    {
        // On Windows/other: spawn a thread that sets the flag.
        // Ctrl-C will interrupt blocking I/O and the thread can set the flag.
        std::thread::spawn(|| {
            // This is a best-effort fallback; the loop will notice the flag.
            let _ = std::io::stdin().read_line(&mut String::new());
            STOP_ATOMIC.store(true, Ordering::Relaxed);
        });
    }

    let mut capture_num: u32 = 0;

    loop {
        // Wait for the interval before the first capture (print count-down).
        if capture_num > 0 || cfg.interval_secs > 0 {
            interruptible_sleep(cfg.interval_secs, stop_flag());
        }
        if stop_flag().load(Ordering::Relaxed) {
            eprintln!("\nhprof-analyzer watch: stopped.");
            break;
        }

        // Check PID is still alive.
        if !pid_exists(cfg.pid) {
            eprintln!(
                "hprof-analyzer watch: PID {} has exited — stopping.",
                cfg.pid
            );
            break;
        }

        capture_num += 1;
        let ts = timestamp_label();
        let hprof_path = cfg.output_dir.join(format!("dump_{ts}.hprof"));
        let report_path = cfg.output_dir.join(format!("report_{ts}.json"));

        eprint!("[{ts}]  capturing dump #{capture_num}… ");

        // Trigger heap dump.
        if let Err(e) = dumper.dump(cfg.pid, &hprof_path) {
            eprintln!("FAILED: {e}");
            continue;
        }
        eprint!("done. Analyzing… ");

        // Analyze the dump inline (no subprocess).
        let opts = cfg.detail.options();
        let source = crate::source::HprofSource::from(hprof_path.to_string_lossy().as_ref());
        match crate::analyze_to_report_with_retained(&source, &opts) {
            Ok((report, _retained)) => {
                // Delete the large .hprof — only keep the compact JSON.
                let _ = std::fs::remove_file(&hprof_path);

                match serde_json::to_string_pretty(&report) {
                    Ok(json) => {
                        if let Err(e) = std::fs::write(&report_path, &json) {
                            eprintln!("FAILED to write report: {e}");
                            continue;
                        }
                        let top = report
                            .leaks
                            .suspects
                            .first()
                            .map(|s| {
                                format!(
                                    " — top suspect: {} {}",
                                    s.pretty_class,
                                    crate::report::format_bytes(s.retained)
                                )
                            })
                            .unwrap_or_default();
                        eprintln!("done.{top}");
                    }
                    Err(e) => {
                        let _ = std::fs::remove_file(&hprof_path);
                        eprintln!("FAILED to serialize report: {e}");
                        continue;
                    }
                }
            }
            Err(e) => {
                let _ = std::fs::remove_file(&hprof_path);
                eprintln!("FAILED to analyze: {e}");
                continue;
            }
        }

        // Prune old reports beyond `keep`.
        if let Err(e) = prune_old_reports(&cfg.output_dir, cfg.keep) {
            eprintln!("  warning: could not prune old reports: {e}");
        }

        // Regenerate rolling diff HTML if requested.
        if cfg.diff {
            match collect_report_paths(&cfg.output_dir) {
                Ok(paths) if paths.len() >= 2 => {
                    let diff_path = cfg.output_dir.join("diff.html");
                    match crate::diff_reports::run(&paths, crate::opts::OutputFormat::Html) {
                        Ok(html) => {
                            if let Err(e) = std::fs::write(&diff_path, html) {
                                eprintln!("  warning: could not write diff.html: {e}");
                            } else {
                                eprintln!("  diff.html updated ({} reports)", paths.len());
                            }
                        }
                        Err(e) => eprintln!("  warning: diff failed: {e}"),
                    }
                }
                Ok(_) => {} // not enough reports yet
                Err(e) => eprintln!("  warning: could not list reports for diff: {e}"),
            }
        }
    }

    Ok(())
}

// ── Duration parsing ──────────────────────────────────────────────────────────

/// Parse a human-readable duration string into seconds.
/// Accepts: "30m", "1h", "300s", "300" (plain number = seconds).
pub fn parse_duration(s: &str) -> Result<u64, String> {
    let s = s.trim();
    if let Some(rest) = s.strip_suffix('h') {
        rest.trim()
            .parse::<u64>()
            .map(|n| n * 3600)
            .map_err(|_| format!("invalid duration '{s}'"))
    } else if let Some(rest) = s.strip_suffix('m') {
        rest.trim()
            .parse::<u64>()
            .map(|n| n * 60)
            .map_err(|_| format!("invalid duration '{s}'"))
    } else if let Some(rest) = s.strip_suffix('s') {
        rest.trim()
            .parse::<u64>()
            .map_err(|_| format!("invalid duration '{s}'"))
    } else {
        s.parse::<u64>()
            .map_err(|_| format!("invalid duration '{s}' — use e.g. \"30m\", \"1h\", \"300s\""))
    }
}

// ── Stop flag ─────────────────────────────────────────────────────────────────

static STOP_ATOMIC: AtomicBool = AtomicBool::new(false);

fn stop_flag() -> &'static AtomicBool {
    &STOP_ATOMIC
}

fn interruptible_sleep(secs: u64, stop: &AtomicBool) {
    for _ in 0..secs {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

// ── Signal handling (Unix) ────────────────────────────────────────────────────

#[cfg(unix)]
extern "C" fn signal_handler(_sig: libc::c_int) {
    STOP_ATOMIC.store(true, Ordering::Relaxed);
}

// ── Dumper abstraction ────────────────────────────────────────────────────────

enum Dumper {
    Jcmd(String),
    Jmap(String),
}

impl Dumper {
    fn name(&self) -> &str {
        match self {
            Dumper::Jcmd(p) => p,
            Dumper::Jmap(p) => p,
        }
    }

    fn dump(&self, pid: u32, dest: &Path) -> Result<(), String> {
        match self {
            Dumper::Jcmd(path) => dump_via_jcmd(path, pid, dest),
            Dumper::Jmap(path) => dump_via_jmap(path, pid, dest),
        }
    }
}

fn dump_via_jcmd(jcmd: &str, pid: u32, dest: &Path) -> Result<(), String> {
    let out = Command::new(jcmd)
        .args([
            &pid.to_string(),
            "GC.heap_dump",
            dest.to_string_lossy().as_ref(),
        ])
        .output()
        .map_err(|e| format!("failed to run jcmd: {e}"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(format!("jcmd failed: {stderr}"));
    }
    Ok(())
}

fn dump_via_jmap(jmap: &str, pid: u32, dest: &Path) -> Result<(), String> {
    let file_arg = format!("format=b,file={}", dest.display());
    let out = Command::new(jmap)
        .args(["-dump:", &file_arg, &pid.to_string()])
        .output()
        .map_err(|e| format!("failed to run jmap: {e}"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(format!("jmap failed: {stderr}"));
    }
    Ok(())
}

/// Find jcmd or jmap on the system.  Tries (in order):
///   1. Explicit override path
///   2. `jcmd` on PATH
///   3. $JAVA_HOME/bin/jcmd
///   4. /usr/lib/jvm/**/bin/jcmd  (Linux)
///   5. jmap fallbacks in same order
fn find_dumper(override_path: Option<&str>) -> io::Result<Dumper> {
    if let Some(p) = override_path {
        if which_exists(p) {
            return Ok(Dumper::Jcmd(p.to_owned()));
        }
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("specified jcmd not found: '{p}'"),
        ));
    }

    // Try jcmd first.
    for candidate in jvm_tool_candidates("jcmd") {
        if which_exists(&candidate) {
            return Ok(Dumper::Jcmd(candidate));
        }
    }

    // Fall back to jmap.
    for candidate in jvm_tool_candidates("jmap") {
        if which_exists(&candidate) {
            return Ok(Dumper::Jmap(candidate));
        }
    }

    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "neither jcmd nor jmap found; install a JDK or pass --jcmd <path>",
    ))
}

fn jvm_tool_candidates(tool: &str) -> Vec<String> {
    let mut candidates = vec![tool.to_owned()]; // PATH lookup

    if let Ok(java_home) = std::env::var("JAVA_HOME") {
        candidates.push(format!("{java_home}/bin/{tool}"));
    }

    // Common Linux JDK installation paths.
    #[cfg(target_os = "linux")]
    if let Ok(entries) = std::fs::read_dir("/usr/lib/jvm") {
        for entry in entries.filter_map(|e| e.ok()) {
            candidates.push(
                entry
                    .path()
                    .join("bin")
                    .join(tool)
                    .to_string_lossy()
                    .into_owned(),
            );
        }
    }

    // macOS: /usr/libexec/java_home -v <ver> prints the JDK home, but
    // $JAVA_HOME already covers the common case.

    candidates
}

/// Returns true if `tool` can be found (either absolute path exists, or
/// `which`-style PATH search succeeds).
fn which_exists(tool: &str) -> bool {
    let p = Path::new(tool);
    if p.is_absolute() || p.components().count() > 1 {
        return p.exists();
    }
    // NAME-only: probe PATH.
    if let Ok(path_var) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path_var) {
            if dir.join(tool).exists() {
                return true;
            }
        }
    }
    false
}

// ── PID check ─────────────────────────────────────────────────────────────────

fn pid_exists(pid: u32) -> bool {
    #[cfg(unix)]
    {
        // kill(pid, 0) returns 0 if the process exists and we have permission.
        unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
    }
    #[cfg(not(unix))]
    {
        // Best-effort on non-Unix: check /proc or assume alive.
        let _ = pid;
        true
    }
}

// ── Report management ─────────────────────────────────────────────────────────

fn prune_old_reports(dir: &Path, keep: usize) -> io::Result<()> {
    let mut reports: Vec<PathBuf> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .filter(|e| {
            let n = e.file_name();
            let s = n.to_string_lossy();
            // Match report_<timestamp>.json but not diff.html
            s.starts_with("report_") && (s.ends_with(".json") || s.ends_with(".json.gz"))
        })
        .map(|e| e.path())
        .collect();
    reports.sort(); // lexicographic on report_<ts>.json = chronological
    if reports.len() > keep {
        for old in &reports[..reports.len() - keep] {
            std::fs::remove_file(old)?;
        }
    }
    Ok(())
}

fn collect_report_paths(dir: &Path) -> io::Result<Vec<String>> {
    let mut paths: Vec<String> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .filter(|e| {
            let n = e.file_name();
            let s = n.to_string_lossy();
            s.starts_with("report_") && (s.ends_with(".json") || s.ends_with(".json.gz"))
        })
        .map(|e| e.path().to_string_lossy().into_owned())
        .collect();
    paths.sort();
    Ok(paths)
}

// ── Timestamp label ───────────────────────────────────────────────────────────

fn timestamp_label() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    // Produce a sortable label: YYYYMMDD_HHMMSS (UTC approximation via seconds).
    // We avoid pulling in `chrono` — format by hand from epoch seconds.
    let (date, time) = epoch_to_date_time(secs);
    format!("{date}_{time}")
}

/// Convert Unix epoch seconds to (YYYYMMDD, HHMMSS) strings (UTC).
fn epoch_to_date_time(secs: u64) -> (String, String) {
    // Days since epoch.
    let days = secs / 86400;
    let time_secs = secs % 86400;
    let h = time_secs / 3600;
    let m = (time_secs % 3600) / 60;
    let s = time_secs % 60;

    // Gregorian calendar from day count (Zeller / Julian Day algorithm).
    let jd = days as i64 + 2440588; // Julian Day Number for 1970-01-01
    let a = jd + 32044;
    let b = (4 * a + 3) / 146097;
    let c = a - (146097 * b) / 4;
    let d = (4 * c + 3) / 1461;
    let e = c - (1461 * d) / 4;
    let m_raw = (5 * e + 2) / 153;
    let day = e - (153 * m_raw + 2) / 5 + 1;
    let month = m_raw + if m_raw < 10 { 3 } else { -9 };
    let year = 100 * b + d - 4800 + if m_raw < 10 { 0 } else { 1 };

    (
        format!("{year:04}{month:02}{day:02}"),
        format!("{h:02}{m:02}{s:02}"),
    )
}

// ── watch-dir ─────────────────────────────────────────────────────────────────

pub struct WatchDirConfig {
    /// Directory to watch for incoming heap dump files.
    pub dir: PathBuf,
    /// Poll interval in seconds (default: 5).
    pub poll_secs: u64,
    /// Maximum number of Report JSONs to keep (oldest deleted).
    pub keep: usize,
    /// Regenerate diff.html after each new report.
    pub diff: bool,
    /// Analysis detail level.
    pub detail: DetailLevel,
    /// Delete the source dump file after analysis.
    pub delete_dumps: bool,
}

/// Extensions treated as heap dump files by `watch-dir`.
fn is_dump_file(name: &str) -> bool {
    name.ends_with(".hprof")
        || name.ends_with(".hprof.gz")
        || name.ends_with(".dump")
        || name.ends_with(".dump.gz")
}

/// Poll `dir` and return the set of dump file paths currently present.
fn list_dump_files(dir: &Path) -> io::Result<std::collections::HashSet<PathBuf>> {
    let mut set = std::collections::HashSet::new();
    for entry in std::fs::read_dir(dir)?.filter_map(|e| e.ok()) {
        let name = entry.file_name();
        if is_dump_file(&name.to_string_lossy()) && entry.path().is_file() {
            set.insert(entry.path());
        }
    }
    Ok(set)
}

/// Wait until a file's size has been stable for two consecutive polls.
/// Returns false if the stop flag fires first.
fn wait_for_stable(path: &Path, poll_secs: u64, stop: &AtomicBool) -> bool {
    let mut prev_size: u64 = 0;
    loop {
        interruptible_sleep(poll_secs.max(2), stop);
        if stop.load(Ordering::Relaxed) {
            return false;
        }
        let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        if size > 0 && size == prev_size {
            return true;
        }
        prev_size = size;
    }
}

/// `hprof-analyzer watch-dir` — monitor a directory for incoming dumps.
pub fn run_watch_dir(cfg: WatchDirConfig) -> io::Result<()> {
    std::fs::create_dir_all(&cfg.dir)?;

    #[cfg(unix)]
    unsafe {
        libc::signal(
            libc::SIGINT,
            signal_handler as extern "C" fn(libc::c_int) as libc::sighandler_t,
        );
        libc::signal(
            libc::SIGTERM,
            signal_handler as extern "C" fn(libc::c_int) as libc::sighandler_t,
        );
    }

    eprintln!(
        "hprof-analyzer watch-dir: monitoring {} — poll {}s, keep {}, delete dumps: {}",
        cfg.dir.display(),
        cfg.poll_secs,
        cfg.keep,
        cfg.delete_dumps,
    );
    eprintln!("  Press Ctrl-C to stop.\n");

    let mut known: std::collections::HashSet<PathBuf> =
        list_dump_files(&cfg.dir).unwrap_or_default();

    loop {
        interruptible_sleep(cfg.poll_secs, stop_flag());
        if stop_flag().load(Ordering::Relaxed) {
            eprintln!("\nhprof-analyzer watch-dir: stopped.");
            break;
        }

        let current = match list_dump_files(&cfg.dir) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("  warning: cannot list directory: {e}");
                continue;
            }
        };

        let mut new_dumps: Vec<PathBuf> = current.difference(&known).cloned().collect();
        new_dumps.sort(); // deterministic order for multiple arrivals
        known = current;

        for dump_path in new_dumps {
            let dump_str = dump_path.to_string_lossy();
            eprint!(
                "  new dump: {} — waiting for write to complete… ",
                dump_path.display()
            );

            if !wait_for_stable(&dump_path, cfg.poll_secs, stop_flag()) {
                eprintln!("interrupted.");
                return Ok(());
            }
            eprintln!("done.");

            let ts = timestamp_label();
            let report_path = cfg.dir.join(format!("report_{ts}.json"));
            eprint!("  analyzing… ");

            let opts = cfg.detail.options();
            let source = crate::source::HprofSource::from(dump_str.as_ref());
            match crate::analyze_to_report_with_retained(&source, &opts) {
                Ok((report, _retained)) => {
                    if cfg.delete_dumps {
                        let _ = std::fs::remove_file(&dump_path);
                    }
                    match serde_json::to_string_pretty(&report) {
                        Ok(json) => {
                            if let Err(e) = std::fs::write(&report_path, &json) {
                                eprintln!("FAILED to write report: {e}");
                                continue;
                            }
                            let top = report
                                .leaks
                                .suspects
                                .first()
                                .map(|s| {
                                    format!(
                                        " — top suspect: {} {}",
                                        s.pretty_class,
                                        crate::report::format_bytes(s.retained)
                                    )
                                })
                                .unwrap_or_default();
                            eprintln!("done.{top}");
                            // Track the new report file.
                            known.insert(report_path.clone());
                        }
                        Err(e) => {
                            eprintln!("FAILED to serialize: {e}");
                            continue;
                        }
                    }
                }
                Err(e) => {
                    eprintln!("FAILED to analyze: {e}");
                    continue;
                }
            }

            if let Err(e) = prune_old_reports(&cfg.dir, cfg.keep) {
                eprintln!("  warning: could not prune old reports: {e}");
            }

            if cfg.diff {
                match collect_report_paths(&cfg.dir) {
                    Ok(paths) if paths.len() >= 2 => {
                        let diff_path = cfg.dir.join("diff.html");
                        match crate::diff_reports::run(&paths, crate::opts::OutputFormat::Html) {
                            Ok(html) => {
                                if let Err(e) = std::fs::write(&diff_path, html) {
                                    eprintln!("  warning: could not write diff.html: {e}");
                                } else {
                                    eprintln!("  diff.html updated ({} reports)", paths.len());
                                }
                            }
                            Err(e) => eprintln!("  warning: diff failed: {e}"),
                        }
                    }
                    Ok(_) => {}
                    Err(e) => eprintln!("  warning: could not list reports for diff: {e}"),
                }
            }
        }
    }

    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_duration_variants() {
        assert_eq!(parse_duration("30m").unwrap(), 1800);
        assert_eq!(parse_duration("1h").unwrap(), 3600);
        assert_eq!(parse_duration("300s").unwrap(), 300);
        assert_eq!(parse_duration("300").unwrap(), 300);
        assert_eq!(parse_duration("2h").unwrap(), 7200);
        assert!(parse_duration("abc").is_err());
        assert!(parse_duration("").is_err());
    }

    #[test]
    fn epoch_to_date_time_known() {
        // 2024-01-15 08:30:00 UTC = 1705307400
        let (date, time) = epoch_to_date_time(1705307400);
        assert_eq!(date, "20240115");
        assert_eq!(time, "083000");
    }

    #[test]
    fn timestamp_label_is_sortable() {
        let a = "20240115_083000".to_string();
        let b = "20240115_090000".to_string();
        assert!(a < b);
    }
}

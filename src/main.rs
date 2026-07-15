//! CLI entry point and two-pass orchestration for the HPROF heap-dump analyzer.
//!
//! Subcommands: `analyze` (parse a dump and emit a report), `render` (re-render
//! a saved Report JSON), `compare mat` (MAT export vs our JSON) / `compare
//! reports` (cross-dump growth), `completions` (shell completion scripts), and
//! `dev` (diagnostics).
//!
//! The `analyze` pipeline runs: pass1 (scan) -> pass2 (build graph) -> compress
//! cold arrays -> rpo DFS -> inbound CSR -> dominators -> retained -> build_model
//! -> render. Allocation/free/compress ordering here is load-bearing for the
//! peak-RSS budget on multi-GB dumps; see the inline notes before changing it.

mod bitset;
mod chunkvec;
mod cvec;
mod diff;
mod diff_reports;
mod dominator;
mod html;
mod id_map;
mod md;
#[cfg(test)]
mod md_test;
mod pass1;
mod pass2;
mod progress;
mod reader;
mod report;
mod retained;
mod rpo_dfs;
mod sweep;
mod trace;
mod types;
mod vbyte;

use std::io::IsTerminal;
use std::{io, process, time::Instant};

use pass1::Pass1;

/// Output format for the analysis report.
#[derive(Clone, Copy, PartialEq)]
enum OutputFormat {
    /// Human-readable Markdown.
    Md,
    /// Markdown with embedded graph/chart blocks.
    MdGraphs,
    /// Canonical Report JSON (deterministic field order).
    Json,
    /// Standalone HTML.
    Html,
}

/// Always-applied per-analysis caps, populated from `--detail`. All four heavy
/// analyses (root paths, alloc sites, thread locals, dominator tree) now run
/// unconditionally; these caps bound their output size. `--detail default`
/// reproduces the historical cap values so MAT/golden parity is unchanged.
#[derive(Clone, Copy)]
pub struct AnalyzeOptions {
    pub root_path_max_depth: usize,
    pub alloc_sites_top: usize,
    pub thread_locals_per_thread: usize,
    pub dominator_tree_max_nodes: usize,
    pub dominator_tree_max_depth: usize,
    pub leak_children_cap: usize,
    pub top_consumers: usize,
}

#[cfg(test)]
impl Default for AnalyzeOptions {
    /// Test-only default: the `--detail default` preset (historical cap values).
    fn default() -> Self {
        DetailLevel::Default.options()
    }
}

use clap::{CommandFactory, Parser, Subcommand, ValueEnum};
use clap_complete::Shell;

/// Top-level CLI: a single subcommand.
#[derive(Parser)]
#[command(
    name = "hprof-analyzer",
    version,
    about = "Analyze Java HPROF heap dumps (Eclipse MAT parity)",
    long_about = "A fast, low-memory analyzer for Java HPROF heap dumps.\n\n\
        It parses a dump in two streaming passes and emits static reports that \
        replicate three Eclipse MAT views — System Overview, Leak Suspects, and \
        Top Consumers — plus a Threads overview. Reports render as plain Markdown, \
        Markdown with ASCII graphs, self-contained HTML, or machine-readable JSON.",
    after_help = "EXAMPLES:\n  \
        hprof-analyzer analyze heap.hprof                 # Markdown to stdout\n  \
        hprof-analyzer analyze heap.hprof report.html     # HTML (format from .html)\n  \
        hprof-analyzer analyze heap.hprof report.json     # JSON  (format from .json)\n  \
        hprof-analyzer analyze heap.hprof -f md-graphs    # Markdown + ASCII graphs\n  \
        hprof-analyzer render report.json report.html     # re-render saved JSON\n  \
        hprof-analyzer compare reports old.json new.json  # cross-dump growth diff\n  \
        hprof-analyzer completions zsh > _hprof-analyzer  # shell completions"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

/// The analyzer subcommands.
#[derive(Subcommand)]
enum Cmd {
    /// Analyze a heap dump and write a report
    #[command(after_help = "EXAMPLES:\n  \
        hprof-analyzer analyze heap.hprof                 # Markdown to stdout\n  \
        hprof-analyzer analyze heap.hprof report.html     # HTML (inferred from .html)\n  \
        hprof-analyzer analyze heap.hprof report.json.gz  # gzip-compressed JSON\n  \
        hprof-analyzer analyze heap.hprof -f md-graphs    # Markdown + ASCII graphs\n  \
        hprof-analyzer analyze heap.hprof --detail max    # looser output caps")]
    Analyze {
        /// Path to the input .hprof heap dump (`.hprof.gz` read transparently).
        input: String,
        /// Output path; writes to stdout when omitted. A `.gz` suffix writes
        /// gzip-compressed. When `--format` is not given, the format is inferred
        /// from this path's extension (.html/.htm, .json[.gz], .md).
        output: Option<String>,
        /// Report output format. Overrides the extension-inferred format;
        /// defaults to Markdown when neither is given.
        #[arg(short, long, value_enum)]
        format: Option<FormatArg>,
        /// Output-size detail preset. `default` reproduces the historical caps;
        /// `minimal` shrinks and `max` expands every per-analysis output cap
        /// (leak-suspect children, dominator subtree, alloc sites, thread
        /// locals, top consumers).
        #[arg(long, value_enum, default_value_t = DetailLevel::Default)]
        detail: DetailLevel,
        /// Log per-phase timing (and RSS on Linux) to stderr.
        #[arg(short, long)]
        verbose: bool,
        /// Emit RSS probe/trim traces at pipeline checkpoints.
        #[arg(long)]
        trace_rss: bool,
        /// Show a live progress line on stderr. `auto` (default) enables it only
        /// when stderr is a terminal and neither --verbose nor --trace-rss is set.
        #[arg(long, value_enum, default_value_t = ProgressWhen::Auto)]
        progress: ProgressWhen,
    },
    /// Re-render a saved canonical Report JSON to another format
    #[command(after_help = "EXAMPLES:\n  \
        hprof-analyzer render report.json                 # Markdown to stdout\n  \
        hprof-analyzer render report.json report.html     # HTML (inferred from .html)\n  \
        hprof-analyzer render report.json.gz -f md-graphs # read .gz, emit md-graphs")]
    Render {
        /// Path to a Report JSON (or `.json.gz`), or "-" for stdin.
        input: String,
        /// Output path; writes to stdout when omitted. A `.gz` suffix writes
        /// gzip-compressed. When `--format` is not given, the format is inferred
        /// from this path's extension (.html/.htm, .json[.gz], .md).
        output: Option<String>,
        /// Report output format. Overrides the extension-inferred format;
        /// defaults to Markdown when neither is given.
        #[arg(short, long, value_enum)]
        format: Option<FormatArg>,
    },
    /// Compare reports (MAT export vs ours, or two of ours across time)
    Compare {
        #[command(subcommand)]
        cmd: CompareCmd,
    },
    /// Generate a shell completion script (write it to your completions dir)
    Completions {
        /// Target shell.
        shell: Shell,
    },
    /// Developer / diagnostic commands
    Dev {
        #[command(subcommand)]
        cmd: DevCmd,
    },
}

/// `compare` subcommands: MAT-parity check, or cross-dump growth.
#[derive(Subcommand)]
enum CompareCmd {
    /// Compare a MAT export against our canonical JSON (exit 2 on FAIL)
    Mat {
        /// Path to the Eclipse MAT report (HTML/zip).
        mat: String,
        /// Path to our canonical Report JSON.
        ours: String,
        /// Diff output format (Markdown or JSON); defaults to Markdown.
        #[arg(short, long, value_enum)]
        format: Option<FormatArg>,
    },
    /// Cross-dump growth: compare two canonical Report JSONs (A=baseline, B=current)
    Reports {
        /// Baseline (earlier) Report JSON, or "-" for stdin.
        a: String,
        /// Current (later) Report JSON.
        b: String,
        /// Diff output format (Markdown or JSON); defaults to Markdown.
        #[arg(short, long, value_enum)]
        format: Option<FormatArg>,
    },
}

/// Developer / diagnostic subcommands.
#[derive(Subcommand)]
enum DevCmd {
    /// Print the JSON Schema of the report model
    EmitSchema,
    /// Aggregate per-dump *.diff.json files into a gate report (exit 2 on gate-fail)
    SweepAggregate { dir: String },
    /// Dump pass-1 parse stats as JSON
    DumpPass1 { input: String },
}

/// CLI mirror of `OutputFormat` (kept separate so clap owns the value-enum).
#[derive(Clone, Copy, PartialEq, ValueEnum)]
enum FormatArg {
    /// Human-readable Markdown.
    Md,
    /// Markdown with embedded graph/chart blocks.
    MdGraphs,
    /// Canonical Report JSON.
    Json,
    /// Standalone HTML.
    Html,
}

/// Output-size preset. `Default` reproduces the historical cap values so
/// MAT/golden parity is unchanged; `Minimal`/`Max` scale the caps down/up.
#[derive(Clone, Copy, PartialEq, ValueEnum)]
enum DetailLevel {
    Minimal,
    Default,
    Max,
}

/// When to show the live progress line on stderr.
#[derive(Clone, Copy, PartialEq, ValueEnum)]
enum ProgressWhen {
    /// Enable only when stderr is a terminal and no verbose/trace flag is set.
    Auto,
    /// Always emit progress lines to stderr.
    Always,
    /// Never emit progress lines.
    Never,
}

impl DetailLevel {
    fn options(self) -> AnalyzeOptions {
        // (root_depth, alloc_top, thread_locals, dom_nodes, dom_depth,
        //  leak_children, top_consumers)
        let (rd, at, tl, dn, dd, lc, tc) = match self {
            DetailLevel::Minimal => (10, 15, 5, 500, 10, 15, 10),
            DetailLevel::Default => (30, 50, 20, 5000, 20, 50, 20),
            DetailLevel::Max => (200, 500, 100, 100_000, 50, 500, 100),
        };
        AnalyzeOptions {
            root_path_max_depth: rd,
            alloc_sites_top: at,
            thread_locals_per_thread: tl,
            dominator_tree_max_nodes: dn,
            dominator_tree_max_depth: dd,
            leak_children_cap: lc,
            top_consumers: tc,
        }
    }
}

impl From<FormatArg> for OutputFormat {
    fn from(f: FormatArg) -> Self {
        match f {
            FormatArg::Md => OutputFormat::Md,
            FormatArg::MdGraphs => OutputFormat::MdGraphs,
            FormatArg::Json => OutputFormat::Json,
            FormatArg::Html => OutputFormat::Html,
        }
    }
}

/// Choose the output format: an explicit `--format` always wins; otherwise
/// infer from the output path's extension; otherwise fall back to Markdown
/// (the stdout default). `md-graphs` is never inferred — it shares the `.md`
/// extension with plain Markdown, so it stays opt-in via `-f md-graphs`.
fn resolve_format(explicit: Option<FormatArg>, out: Option<&str>) -> OutputFormat {
    if let Some(f) = explicit {
        return f.into();
    }
    if let Some(path) = out {
        let lower = path.to_ascii_lowercase();
        if lower.ends_with(".html") || lower.ends_with(".htm") {
            return OutputFormat::Html;
        }
        if lower.ends_with(".json") || lower.ends_with(".json.gz") {
            return OutputFormat::Json;
        }
        // .md / .markdown (and anything else) → plain Markdown.
    }
    OutputFormat::Md
}

/// Write report text to `path`, or to stdout when `path` is `None`. A `.gz`
/// suffix is written gzip-compressed (matching how `render` reads it back).
fn write_output(path: Option<&str>, text: &str) -> io::Result<()> {
    match path {
        Some(p) if p.ends_with(".gz") => {
            use std::io::Write;
            let f = std::fs::File::create(p).map_err(|e| io::Error::new(e.kind(), e))?;
            let mut enc = flate2::write::GzEncoder::new(f, flate2::Compression::best());
            enc.write_all(text.as_bytes())?;
            enc.finish()?;
            Ok(())
        }
        Some(p) => std::fs::write(p, text).map_err(|e| io::Error::new(e.kind(), e)),
        None => {
            print!("{text}");
            Ok(())
        }
    }
}

/// Print a one-line `error:` message to stderr and exit with status 1.
fn fail(msg: impl std::fmt::Display) -> ! {
    eprintln!("error: {msg}");
    process::exit(1);
}

/// Parse args and dispatch to the selected subcommand.
fn main() {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Analyze {
            input,
            output,
            format,
            detail,
            verbose,
            trace_rss,
            progress,
        } => {
            if trace_rss {
                trace::set_enabled(true);
            }
            // Progress: `auto` shows a live line only on an interactive stderr,
            // and never when --verbose/--trace-rss already print phase lines.
            let show_progress = match progress {
                ProgressWhen::Always => true,
                ProgressWhen::Never => false,
                ProgressWhen::Auto => !verbose && !trace_rss && std::io::stderr().is_terminal(),
            };
            progress::set_enabled(show_progress);
            let fmt = resolve_format(format, output.as_deref());
            let opts = detail.options();
            if let Err(e) = run(
                &input,
                output.as_deref(),
                fmt,
                verbose,
                cvec::Codec::Deflate9,
                opts,
            ) {
                fail(analyze_error_hint(&input, &e));
            }
        }
        Cmd::Render {
            input,
            output,
            format,
        } => {
            let fmt = resolve_format(format, output.as_deref());
            match render_report(&input, fmt) {
                Ok(text) => {
                    if let Err(e) = write_output(output.as_deref(), &text) {
                        fail(e);
                    }
                }
                Err(e) => fail(render_error_hint(&input, &e)),
            }
        }
        Cmd::Compare { cmd } => match cmd {
            CompareCmd::Mat { mat, ours, format } => {
                let json_out = resolve_format(format, None) == OutputFormat::Json;
                match diff::run_diff(&mat, &ours, json_out) {
                    Ok(true) => {}
                    Ok(false) => process::exit(2),
                    Err(e) => fail(e),
                }
            }
            CompareCmd::Reports { a, b, format } => {
                match diff_reports::run(&a, &b, resolve_format(format, None)) {
                    Ok(text) => print!("{text}"),
                    Err(e) => fail(e),
                }
            }
        },
        Cmd::Completions { shell } => {
            let mut cmd = Cli::command();
            clap_complete::generate(shell, &mut cmd, "hprof-analyzer", &mut io::stdout());
        }
        Cmd::Dev { cmd } => match cmd {
            DevCmd::EmitSchema => {
                let schema = schemars::schema_for!(report::Report);
                match serde_json::to_string_pretty(&schema) {
                    Ok(js) => println!("{js}"),
                    Err(e) => fail(e),
                }
            }
            DevCmd::SweepAggregate { dir } => match sweep::run_aggregate(&dir) {
                Ok(true) => {}
                Ok(false) => process::exit(2),
                Err(e) => fail(e),
            },
            DevCmd::DumpPass1 { input } => {
                if let Err(e) = dump_pass1_json(&input) {
                    fail(e);
                }
            }
        },
    }
}

/// Turn an `analyze` pipeline error into an actionable message. A missing input
/// file is the most common mistake, so name the path explicitly.
fn analyze_error_hint(input: &str, e: &io::Error) -> String {
    if e.kind() == io::ErrorKind::NotFound {
        format!("cannot open '{input}': no such file or directory")
    } else {
        e.to_string()
    }
}

/// Turn a `render` error into an actionable message. The classic mistake is
/// pointing `render` at a heap dump instead of a saved Report JSON.
fn render_error_hint(input: &str, e: &io::Error) -> String {
    if e.kind() == io::ErrorKind::NotFound {
        return format!("cannot open '{input}': no such file or directory");
    }
    if looks_like_hprof(input) {
        return format!(
            "'{input}' looks like a heap dump, not a Report JSON; use `analyze`, not `render`"
        );
    }
    e.to_string()
}

/// True when the file at `path` begins with the HPROF magic (`JAVA PROFILE`).
fn looks_like_hprof(path: &str) -> bool {
    use std::io::Read;
    if path == "-" {
        return false;
    }
    let Ok(mut f) = std::fs::File::open(path) else {
        return false;
    };
    let mut head = [0u8; 12];
    matches!(f.read_exact(&mut head), Ok(())) && head.starts_with(b"JAVA PROFILE")
}

/// Read current process RSS from /proc/self/status (Linux only).
/// Returns 0 on any error or non-Linux platform.
fn rss_mb() -> f64 {
    #[cfg(target_os = "linux")]
    {
        if let Ok(s) = std::fs::read_to_string("/proc/self/status") {
            for line in s.lines() {
                if let Some(rest) = line.strip_prefix("VmRSS:") {
                    let kb: u64 = rest
                        .split_whitespace()
                        .next()
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(0);
                    return kb as f64 / 1024.0;
                }
            }
        }
    }
    0.0
}

/// Log a phase name, elapsed seconds, and (Linux) RSS when `verbose`.
fn log(verbose: bool, phase: &str, elapsed: f64) {
    if verbose {
        let rss = rss_mb();
        if rss > 0.0 {
            eprintln!("{phase}: {elapsed:.2}s  RSS={rss:.0} MB");
        } else {
            eprintln!("{phase}: {elapsed:.2}s");
        }
    }
}

/// Re-render a previously saved canonical Report JSON to the given format.
/// The input may be gzip-compressed (`.json.gz`): decompression is transparent,
/// detected by the gzip magic bytes so it works for files and stdin alike.
fn render_report(path: &str, format: OutputFormat) -> io::Result<String> {
    use std::io::Read;
    let raw = if path == "-" {
        let mut buf = Vec::new();
        io::stdin().read_to_end(&mut buf)?;
        buf
    } else {
        std::fs::read(path)?
    };
    // gzip magic (0x1f 0x8b): decompress transparently, matching how the
    // analyzer already reads `.hprof.gz` dumps.
    let json = if raw.starts_with(&[0x1f, 0x8b]) {
        let mut d = flate2::read::GzDecoder::new(&raw[..]);
        let mut s = String::new();
        d.read_to_string(&mut s)?;
        s
    } else {
        String::from_utf8(raw).map_err(|e| {
            io::Error::new(io::ErrorKind::InvalidData, format!("input not UTF-8: {e}"))
        })?
    };
    let report: report::Report = serde_json::from_str(&json).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid report JSON: {e}"),
        )
    })?;
    if report.schema_version != report::SCHEMA_VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "report schema_version {} does not match supported version {}; refusing to render",
                report.schema_version,
                report::SCHEMA_VERSION
            ),
        ));
    }
    Ok(match format {
        OutputFormat::Md => report::render_markdown(&report),
        OutputFormat::MdGraphs => report::render_markdown_graphs(&report),
        OutputFormat::Json => serde_json::to_string_pretty(&report).map_err(io::Error::other)?,
        OutputFormat::Html => html::render_html(&report),
    })
}

/// Run the full `analyze` pipeline end-to-end and write the report.
/// Phase order and the interleaved allocation/free/compress steps are tuned
/// for the peak-RSS budget; the inline comments flag the load-bearing points.
fn run(
    input: &str,
    output: Option<&str>,
    format: OutputFormat,
    verbose: bool,
    compress: cvec::Codec,
    opts: AnalyzeOptions,
) -> io::Result<()> {
    let t_total = Instant::now();

    let t = Instant::now();
    progress::phase("scanning dump (pass 1)");
    let p1 = pass1::Pass1::run(input)?;
    log(verbose, "pass1", t.elapsed().as_secs_f64());

    // The entire analysis works in u32 pre-order / node-index space (dfn,
    // vertex, forward/inbound CSR, idom). A dump with more than u32::MAX
    // objects would silently overflow every index, so refuse it up front with
    // a clear message rather than emit corrupt results.
    if p1.class_ids.len() > u32::MAX as usize {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "dump has {} objects, exceeding the {} (u32::MAX) limit of the \
                 analyzer's index scheme; cannot analyze",
                p1.class_ids.len(),
                u32::MAX
            ),
        ));
    }

    let t = Instant::now();
    progress::phase("building object graph (pass 2)");
    let (mut g, mut inbound, shallow_c, class_idx_c, alloc_serial_c) =
        pass2::Pass2::build(input, p1, compress, &opts)?;
    log(
        verbose,
        &format!("pass2 n={}", g.n),
        t.elapsed().as_secs_f64(),
    );

    // Compress the three cold arrays (shallow, class_idx, id_map) that sit idle
    // across the rpo -> inbound -> dominator peak window, freeing their dense
    // Vecs and holding only small blobs. Restored just before each consumer.
    let t = Instant::now();
    // Compress id_map FIRST: it is the largest cold array (~4.1GB dense u64)
    // and sits dense atop the ~6GB fwd CSR while shallow/class_idx compress.
    // The compress-cold RSS max is during shallow's compression, so freeing
    // id_map's 4.1GB before that removes it from the binding peak. id_map is
    // delta-vbyte+deflate (sorted addrs, fast), not a slow permutation deflate.
    inbound.compress_id_map(compress)?;
    // shallow/class_idx were already compressed inside pass2 (before the
    // fwd_targets alloc) to keep their ~4GB dense forms off the binding peak;
    // shallow_c/class_idx_c hold the blobs, g.shallow/g.class_idx are empty.
    log(verbose, "compress-cold", t.elapsed().as_secs_f64());

    let t = Instant::now();
    progress::phase("ordering objects (reverse post-order)");
    let rpo = rpo_dfs::rpo_dfs(g.n, &g.gc_root_indices, &g.fwd_offsets, &g.fwd_targets);
    log(verbose, "rpo", t.elapsed().as_secs_f64());

    // Free forward CSR (no longer needed after DFS)
    g.fwd_offsets = Vec::new();
    g.fwd_targets = Vec::new();
    crate::trace::trim();

    // Build the inbound CSR now that rpo has freed its arrays — keeps the
    // ~5.5GB inbound CSR off the rpo-phase RSS peak.
    // build() translates inbound predecessors into pre-order space using dfn,
    // so dominator Phase 1 no longer needs dfn. Free dfn immediately after,
    // BEFORE dominator's Phase-1 peak (semi/ancestor/label + rpo + inbound all
    // resident) — this is the binding global peak; dropping dfn cuts ~2GB.
    let mut rpo = rpo;
    let t = Instant::now();
    progress::phase("building inbound references");
    let (inb_block_off, inb_data) = inbound.build(&rpo.dfn)?;
    log(verbose, "inbound", t.elapsed().as_secs_f64());
    // Rebuild vertex now: dfn is still live and inbound.build (the binding-peak
    // 2b scan) has returned, so the 1.96GB vertex never coexists with inb_flat.
    // vertex = invert(dfn) is a pure O(n) pass; the dominator reads it next.
    let count = rpo.parent_pre.len();
    rpo.vertex = rpo_dfs::rebuild_vertex(&rpo.dfn, count);
    crate::trace::probe("main: after rebuild_vertex (post-inbound, dfn live)");
    rpo.dfn = Vec::new();
    crate::trace::trim();

    let t = Instant::now();
    progress::phase("computing dominators");
    // rpo moved by value; vertex/parent_pre owned through translation. dfn
    // already freed above. No separate drop(rpo).
    g.idom =
        dominator::compute_dominators(g.n, rpo, &g.gc_root_indices, &inb_block_off, &inb_data)?;
    log(verbose, "dominator", t.elapsed().as_secs_f64());
    // The inbound (referrer) CSR is consumed by the dominator and never read
    // again: root paths derive their GC-root chains from `g.idom` (the
    // dominator tree, which MAT also uses), so there is no need to preserve or
    // compress the ~7.5GB CSR + vertex map. Free it immediately for every run.
    drop(inb_block_off);
    drop(inb_data);
    crate::trace::trim();

    // Build the dominator-children CSR ONCE and share it across compute_retained
    // (hasSame DFS) and report::leak_suspects (both previously rebuilt it, ~6GB
    // redundant @514M). Built BEFORE restoring shallow/class_idx: the build's
    // transient (child_deg+child_off+child_tgt ~8GB, child_deg freed inside)
    // must not coexist with the 4GB dense shallow+class_idx -> that stacking
    // was the ~22GB global peak. It reads only idom.
    crate::trace::probe("main: before build_dom_children_csr");
    let (dc_off, dc_tgt) = retained::build_dom_children_csr(g.n, &g.idom);
    crate::trace::probe("main: after build_dom_children_csr");

    // Restore shallow/class_idx now that the CSR-build transient has freed
    // child_deg (dominator already freed the inbound CSR too).
    if compress != cvec::Codec::None {
        g.shallow = shallow_c.restore()?;
        g.class_idx = class_idx_c.restore()?;
    }
    drop(shallow_c);
    drop(class_idx_c);
    crate::trace::probe("main: after restore shallow/class_idx");

    let t = Instant::now();
    progress::phase("computing retained sizes");
    let class_count = g.class_names.len();
    let (retained, has_same, depth_counts) = retained::compute_retained(
        g.n,
        &g.idom,
        &g.shallow,
        &g.class_idx,
        class_count,
        &g.class_obj_class_idx,
        &dc_off,
        &dc_tgt,
    );
    g.retained = retained;
    g.has_same_class_ancestor = has_same;
    log(verbose, "retained", t.elapsed().as_secs_f64());

    // Restore + aggregate + free the alloc stack serials in a bounded window
    // right after compute_retained (needs g.shallow + g.retained, both live
    // now). RSS here is well below the rpo/inbound/dominator binding peak, so
    // the transient decode buffer stays under it. We decompress to the raw
    // u32-byte buffer and aggregate by STREAMING over it (no second ~2GB
    // Vec<u32>): restore() would hold both the decompressed bytes AND the
    // collected Vec (~4GB transient — the spike that defeated the naive
    // placement). Only the KB-scale AllocSites summary is carried into
    // build_model, so the report phase never holds the per-object array.
    let alloc_sites = if let Some(c) = alloc_serial_c {
        // Stream the deflate blob through a 64 KiB scratch buffer, feeding each
        // serial into the accumulator in index order. Never materialises the
        // ~2GB decompressed byte buffer OR a collected Vec<u32> — the transient
        // is O(64 KiB), well under the binding rpo peak.
        let mut agg = report::AllocAgg::new(&g, opts.alloc_sites_top);
        c.for_each_u32(|serial| agg.push(serial))?;
        let a = agg.finish();
        g.alloc_frames_by_serial = None;
        crate::trace::trim();
        Some(a)
    } else {
        // Codec::None path (never taken on the big dump): aggregate directly.
        let a = report::build_alloc_sites(&g, opts.alloc_sites_top);
        g.alloc_stack_serial = Vec::new();
        g.alloc_frames_by_serial = None;
        Some(a)
    };

    let t = Instant::now();
    progress::phase("building report");
    crate::trace::probe("report: before build_model");
    // build_model reads has_same_class_ancestor (system-overview group) and
    // dc_off/dc_tgt (leak-suspect group) and stores only bounded aggregates,
    // so both can be freed immediately after it returns. depth_counts is the
    // B2 dominator-depth histogram tallied during compute_retained's DFS (no
    // separate ~2GB per-object memo scan).
    let report = report::build_model(
        &g,
        &dc_off,
        &dc_tgt,
        opts.leak_children_cap,
        &depth_counts,
        &opts,
        alloc_sites,
    );
    crate::trace::probe("report: after build_model");
    g.has_same_class_ancestor = crate::bitset::Bitset::default(); // consumed by build_model
    drop(dc_off);
    drop(dc_tgt);
    crate::trace::trim();
    let out_text = match format {
        OutputFormat::Md => {
            let md = report::render_markdown(&report);
            crate::trace::probe("report: after render_markdown");
            md
        }
        OutputFormat::MdGraphs => {
            let md = report::render_markdown_graphs(&report);
            crate::trace::probe("report: after render_markdown_graphs");
            md
        }
        OutputFormat::Json => {
            // serde_json over a struct preserves field declaration order and
            // carries no f64 (pct is #[serde(skip)]), so output is
            // deterministic. The model holds only KB-scale aggregates, so
            // serialization is trivially RSS-safe even for huge dumps.
            let js = serde_json::to_string_pretty(&report).map_err(io::Error::other)?;
            crate::trace::probe("report: after serialize_json");
            js
        }
        OutputFormat::Html => {
            let h = html::render_html(&report);
            crate::trace::probe("report: after render_html");
            h
        }
    };
    log(verbose, "report", t.elapsed().as_secs_f64());

    // Clear the progress line before emitting output, so it does not linger on
    // stderr next to the report (or leak into a piped tail).
    progress::done();
    write_output(output, &out_text)?;

    log(verbose, "total", t_total.elapsed().as_secs_f64());
    Ok(())
}

/// Emit pass-1 parse stats (counts + class histogram) as JSON to stdout.
fn dump_pass1_json(path: &str) -> io::Result<()> {
    let p = Pass1::run(path)?;

    let mut class_hist: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
    for (i, &cidx) in p.class_ids.iter().enumerate() {
        // class_ids holds interned indices; resolve to addr for kinds that
        // reference a class object (0=instance, 3=class-obj). arrays skip.
        if p.kind[i] != 0 && p.kind[i] != 3 {
            continue;
        }
        let addr = p.class_addr_table[cidx as usize];
        if let Some(ci) = p.class_map.get(&addr) {
            let name = p
                .strings
                .get(&ci.name_id)
                .cloned()
                .unwrap_or_else(|| format!("unknown@{addr:#x}"));
            *class_hist.entry(name).or_insert(0) += 1;
        }
    }

    let mut unique_roots: std::collections::HashSet<u64> = std::collections::HashSet::new();
    for &a in &p.gc_root_addrs {
        unique_roots.insert(a);
    }

    print!("{{");
    print!(r#""id_size":{}"#, p.id_size);
    print!(r#","format":"{}""#, p.format);
    print!(r#","instances":{}"#, p.instance_count);
    print!(r#","obj_arrays":{}"#, p.obj_array_count);
    print!(r#","prim_arrays":{}"#, p.prim_array_count);
    print!(r#","classes":{}"#, p.class_dump_count);
    print!(r#","gc_roots_total":{}"#, p.gc_root_addrs.len());
    print!(r#","strings":{}"#, p.strings.len());

    print!(r#","class_histogram":{{"#);
    let mut first = true;
    for (name, count) in &class_hist {
        if !first {
            print!(",");
        }
        let escaped = name.replace('\\', "\\\\").replace('"', "\\\"");
        print!(r#""{escaped}":{count}"#);
        first = false;
    }
    print!("}}");

    println!("}}");
    Ok(())
}

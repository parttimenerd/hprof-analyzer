//! CLI entry point and two-pass orchestration for the HPROF heap-dump analyzer.
//!
//! The default (no-subcommand) form sniffs the positional input: a `.hprof[.gz]`
//! dump (or HPROF magic) runs the analyze pipeline, anything else is re-rendered
//! as a saved Report JSON. Named subcommands: `compare mat` (MAT export vs our
//! JSON) / `compare reports` (cross-dump growth), `completions` (shell completion
//! scripts), and `dev` (diagnostics).
//!
//! The analyze pipeline runs: pass1 (scan) -> pass2 (build graph) -> compress
//! cold arrays -> rpo DFS -> inbound CSR -> dominators -> retained -> build_model
//! -> render. Allocation/free/compress ordering here is load-bearing for the
//! peak-RSS budget on multi-GB dumps; see the inline notes before changing it.

mod bitset;
mod chunkvec;
mod collection_config;
mod cvec;
mod diff;
mod diff_reports;
mod dominator;
mod html;
mod id_map;
mod mat;
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
mod unreachable_retained;
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
#[derive(Clone)]
pub struct AnalyzeOptions {
    pub root_path_max_depth: usize,
    pub alloc_sites_top: usize,
    pub thread_locals_per_thread: usize,
    pub dominator_tree_max_nodes: usize,
    pub dominator_tree_max_depth: usize,
    pub leak_children_cap: usize,
    pub top_consumers: usize,
    pub find_duplicates: bool,
    pub collections: bool,
    pub collection_config: Option<std::path::PathBuf>,
    pub(crate) coll_descs: Vec<crate::pass2::CollDesc>,
    /// Store field-name labels on forward edges so root-path steps show
    /// `ParentClass.fieldName → ChildClass`. Gated: adds ~2 bytes per edge
    /// (~100–500 MB extra RSS on multi-GB dumps).
    pub ref_paths: bool,
    /// Skip build_model + render. Used by `mat caches` which discards the report.
    pub skip_report: bool,
}

#[cfg(test)]
impl Default for AnalyzeOptions {
    /// Test-only default: the `--detail default` preset (historical cap values).
    fn default() -> Self {
        DetailLevel::Default.options()
    }
}

use clap::{CommandFactory, Parser, Subcommand, ValueEnum, ValueHint};
use clap_complete::Shell;

/// Analyze a heap dump or re-render a saved report. The input is sniffed:
/// a `.hprof[.gz]` dump (or any file starting with the HPROF magic) runs the
/// full analysis pipeline; anything else is treated as a saved Report JSON and
/// re-rendered.
#[derive(Parser)]
#[command(
    name = "hprof-analyzer",
    version,
    about = "Analyze Java HPROF heap dumps (Eclipse MAT parity)",
    long_about = "A fast, low-memory analyzer for Java HPROF heap dumps.\n\n\
        Give it a heap dump and it parses the dump in a few streaming passes and \
        emits static reports that replicate three Eclipse MAT views: System \
        Overview, Leak Suspects, and Top Consumers, plus a Threads overview and \
        some extended collection views. Give it a saved Report JSON instead and \
        it re-renders that report without re-parsing the dump. Reports render as \
        plain Markdown, Markdown with ASCII graphs, self-contained HTML, or \
        machine-readable JSON.",
    after_help = "EXAMPLES:\n  \
        hprof-analyzer heap.hprof                         # Markdown to stdout\n  \
        hprof-analyzer heap.hprof report.html             # HTML (format from .html)\n  \
        hprof-analyzer heap.hprof report.json             # JSON (format from .json)\n  \
        hprof-analyzer heap.hprof -f md-graphs            # Markdown + ASCII graphs\n  \
        hprof-analyzer report.json report.html            # re-render saved JSON to HTML\n  \
        hprof-analyzer compare reports r1.json r2.json [r3.json …]  # cross-dump growth diff\n  \
        hprof-analyzer completions zsh > _hprof-analyzer  # shell completions\n\n\
        Install zsh completions:\n  \
        hprof-analyzer completions zsh > \"${fpath[1]}/_hprof-analyzer\"",
    args_conflicts_with_subcommands = true
)]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,

    /// A `.hprof[.gz]` heap dump to analyze, or a saved Report JSON (or
    /// `.json.gz`, or `-` for stdin) to re-render. Required when no subcommand
    /// is given.
    #[arg(value_hint = ValueHint::FilePath)]
    input: Option<String>,

    /// Output path; writes to stdout when omitted. A `.gz` suffix writes
    /// gzip-compressed. When `--format` is not given, the format is inferred
    /// from this path's extension (.html/.htm, .json[.gz], .md).
    #[arg(value_hint = ValueHint::AnyPath)]
    output: Option<String>,

    /// Report output format. Overrides the extension-inferred format;
    /// defaults to Markdown when neither is given.
    #[arg(short, long, value_enum)]
    format: Option<FormatArg>,

    /// Output-size detail preset. `default` reproduces the historical caps;
    /// `minimal` shrinks and `max` expands every per-analysis output cap
    /// (leak-suspect children, dominator subtree, alloc sites, thread
    /// locals, top consumers). Ignored when re-rendering a saved report.
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

    /// Compute approximate duplicate-object analysis: finds content-identical
    /// `java.lang.String` values and content-identical primitive arrays
    /// (byte[], int[], etc.), then reports wasted bytes and top offenders.
    /// Hashes content to 64 bits — never retains the raw data, so RSS stays
    /// bounded. Adds a few extra heap-file scans; off by default. Analyze-only.
    #[arg(long)]
    find_duplicates: bool,

    /// Compute container attribution by holder Class#field (opt-in; adds
    /// ~300MB peak RSS). Analyze-only.
    #[arg(long)]
    collections: bool,

    /// Path to a TOML file defining custom collection handlers.
    /// Auto-discovers .hprof-analyzer.toml (CWD) or $HOME/.config/hprof-analyzer/collections.toml.
    #[arg(long, value_name = "PATH")]
    collection_config: Option<std::path::PathBuf>,

    /// Store field-name labels on forward edges so that leak-suspect
    /// "path to GC roots" steps show `ParentClass.fieldName → ChildClass`.
    /// Gated (off by default): adds ~2 bytes per reference edge in the heap
    /// (~100–500 MB extra RSS for large dumps). Analyze-only.
    #[arg(long)]
    ref_paths: bool,

    /// Emit Eclipse MAT-compatible binary index files into DIR while running
    /// the normal analysis (the report output is unaffected). The files are
    /// named `<dump>.<kind>.index` using the input basename as the prefix.
    /// Analyze-only; adds some transient RSS to re-materialize compressed arrays.
    #[arg(long, value_name = "DIR", value_hint = ValueHint::DirPath)]
    mat: Option<std::path::PathBuf>,

    /// Path to the MemoryAnalyzer executable (used with --mat to auto-detect
    /// the MAT plugins directory and resolve the correct parser ID).
    #[arg(long, value_hint = ValueHint::FilePath)]
    mat_binary: Option<std::path::PathBuf>,
}

/// Named subcommands. The default (no subcommand) analyzes or re-renders the
/// positional input; see `Cli`.
#[derive(Subcommand)]
enum Cmd {
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
    /// Eclipse MAT cache generation
    Mat {
        #[command(subcommand)]
        cmd: MatCmd,
    },
}

/// `mat` subcommands.
#[derive(Subcommand)]
enum MatCmd {
    /// Generate Eclipse MAT-compatible binary index files for a heap dump.
    ///
    /// Runs the full analysis pipeline on HPROF and writes the MAT cache files
    /// into DIR (default: same directory as the dump). The files are named
    /// `<dump>.<kind>.index` matching MAT's own naming convention. After this
    /// completes, Eclipse MAT can open the dump instantly without re-parsing.
    Caches {
        /// The `.hprof[.gz]` heap dump to analyze.
        #[arg(value_hint = ValueHint::FilePath)]
        input: String,
        /// Directory to write the MAT index files into. Defaults to the
        /// directory containing the heap dump.
        #[arg(value_hint = ValueHint::DirPath)]
        dir: Option<String>,
        /// Path to the MemoryAnalyzer executable. Used to auto-detect the
        /// MAT plugins directory and resolve the correct parser ID for the
        /// `.index` header. When omitted, common installation paths are tried.
        #[arg(long, value_hint = ValueHint::FilePath)]
        mat_binary: Option<String>,
        /// Emit RSS trace lines to stderr (useful for memory profiling).
        #[arg(long)]
        trace_rss: bool,
    },
}

/// `compare` subcommands: MAT-parity check, or cross-dump growth.
#[derive(Subcommand)]
enum CompareCmd {
    /// Compare a MAT export against our canonical JSON (exit 2 on FAIL)
    Mat {
        /// Path to the Eclipse MAT report (HTML/zip).
        #[arg(value_hint = ValueHint::FilePath)]
        mat: String,
        /// Path to our canonical Report JSON.
        #[arg(value_hint = ValueHint::FilePath)]
        ours: String,
        /// Diff output format (Markdown or JSON); defaults to Markdown.
        #[arg(short, long, value_enum)]
        format: Option<FormatArg>,
    },
    /// Cross-dump growth: compare 2+ canonical Report JSONs as a time series
    /// (first = baseline, last = current)
    Reports {
        /// Report JSON paths in time order (first = baseline). Two or more are
        /// required; use "-" for stdin (at most one).
        #[arg(value_hint = ValueHint::FilePath, num_args = 2..)]
        reports: Vec<String>,
        /// Diff output format (Markdown, JSON, or HTML); defaults to Markdown.
        #[arg(short, long, value_enum)]
        format: Option<FormatArg>,
        /// Write output to this path instead of stdout. Gzip-compressed when the
        /// path ends in `.gz`.
        #[arg(short, long, value_hint = ValueHint::FilePath)]
        output: Option<String>,
    },
}

/// Developer / diagnostic subcommands.
#[derive(Subcommand)]
enum DevCmd {
    /// Print the JSON Schema of the report model
    EmitSchema,
    /// Aggregate per-dump *.diff.json files into a gate report (exit 2 on gate-fail)
    SweepAggregate {
        #[arg(value_hint = ValueHint::DirPath)]
        dir: String,
    },
    /// Dump pass-1 parse stats as JSON
    DumpPass1 {
        #[arg(value_hint = ValueHint::FilePath)]
        input: String,
    },
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
            find_duplicates: false,
            collections: false,
            collection_config: None,
            coll_descs: Vec::new(),
            ref_paths: false,
            skip_report: false,
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
    // Restore default SIGPIPE handling so `… | head` (or any reader that closes
    // early) terminates us via the signal like a normal Unix filter, instead of
    // Rust's default SIG_IGN turning the closed pipe into an EPIPE that panics
    // on the next stdout write. Unix only; a no-op elsewhere.
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }

    let cli = Cli::parse();
    match cli.cmd {
        None => run_default(cli),
        Some(Cmd::Compare { cmd }) => match cmd {
            CompareCmd::Mat { mat, ours, format } => {
                // Name a missing input up front — `run_diff` opens both files but
                // surfaces only a bare OS error, so pre-check for a clear message.
                for p in [&mat, &ours] {
                    if p != "-" && !std::path::Path::new(p).exists() {
                        fail(format!("cannot open '{p}': no such file or directory"));
                    }
                }
                let json_out = resolve_format(format, None) == OutputFormat::Json;
                match diff::run_diff(&mat, &ours, json_out) {
                    Ok(true) => {}
                    Ok(false) => process::exit(2),
                    Err(e) => fail(e),
                }
            }
            CompareCmd::Reports { reports, format, output } => {
                // Name a missing input up front for a clear error, mirroring the
                // MAT arm. Skip "-" (stdin) — it has no filesystem path.
                for p in &reports {
                    if p != "-" && !std::path::Path::new(p).exists() {
                        fail(format!("cannot open '{p}': no such file or directory"));
                    }
                }
                match diff_reports::run(&reports, resolve_format(format, None)) {
                    Ok(text) => {
                        if let Some(path) = output {
                            let bytes = text.into_bytes();
                            if path.ends_with(".gz") {
                                use std::io::Write;
                                match std::fs::File::create(&path) {
                                    Ok(f) => {
                                        let mut gz = flate2::write::GzEncoder::new(f, flate2::Compression::default());
                                        if let Err(e) = gz.write_all(&bytes).and_then(|_| gz.finish().map(|_| ())) {
                                            fail(format!("gzip write error: {e}"));
                                        }
                                    }
                                    Err(e) => fail(format!("cannot create '{path}': {e}")),
                                }
                            } else if let Err(e) = std::fs::write(&path, &bytes) {
                                fail(format!("cannot write '{path}': {e}"));
                            }
                        } else {
                            print!("{text}");
                        }
                    }
                    Err(e) => fail(e),
                }
            }
        },
        Some(Cmd::Completions { shell }) => {
            let mut cmd = Cli::command();
            clap_complete::generate(shell, &mut cmd, "hprof-analyzer", &mut io::stdout());
        }
        Some(Cmd::Dev { cmd }) => match cmd {
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
        Some(Cmd::Mat { cmd }) => match cmd {
            MatCmd::Caches { input, dir, mat_binary, trace_rss } => {
                if !input_is_hprof(&input) {
                    fail(format!("'{input}' does not look like a .hprof[.gz] file"));
                }
                if trace_rss {
                    trace::set_enabled(true);
                }
                progress::set_enabled(std::io::stderr().is_terminal() && !trace_rss);
                let mat_dir = dir.as_deref().unwrap_or_else(|| {
                    std::path::Path::new(&input)
                        .parent()
                        .and_then(|p| p.to_str())
                        .unwrap_or(".")
                });
                let base = std::path::Path::new(&input)
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("dump");
                let prefix = base
                    .strip_suffix(".hprof.gz")
                    .or_else(|| base.strip_suffix(".hprof"))
                    .unwrap_or(base);
                let mat_bin_path = mat_binary.as_deref().map(std::path::Path::new);
                let mat_emitter = match mat::MatEmitter::new(std::path::Path::new(mat_dir), prefix, mat_bin_path) {
                    Ok(e) => e,
                    Err(e) => fail(format!("cannot create MAT index dir '{mat_dir}': {e}")),
                };
                if let Err(e) = run(
                    &input,
                    Some("/dev/null"),
                    OutputFormat::Md,
                    false,
                    cvec::Codec::Zstd3,
                    AnalyzeOptions { skip_report: true, ..DetailLevel::Default.options() },
                    Some(mat_emitter),
                ) {
                    fail(e);
                }
            }
        },
    }
}

/// The default (no-subcommand) command: sniff the input and either run the
/// full analyze pipeline (HPROF) or re-render a saved Report JSON.
fn run_default(cli: Cli) {
    let Some(input) = cli.input else {
        // No subcommand and no input: this is a usage error, so write help to
        // stderr (not stdout) and exit 2, matching clap's own missing-arg path.
        let mut cmd = Cli::command();
        let _ = cmd.write_help(&mut io::stderr());
        eprintln!();
        process::exit(2);
    };

    if input_is_hprof(&input) {
        if cli.trace_rss {
            trace::set_enabled(true);
        }
        let show_progress = match cli.progress {
            ProgressWhen::Always => true,
            ProgressWhen::Never => false,
            ProgressWhen::Auto => !cli.verbose && !cli.trace_rss && std::io::stderr().is_terminal(),
        };
        progress::set_enabled(show_progress);
        let fmt = resolve_format(cli.format, cli.output.as_deref());
        let opts = cli.detail.options();
        let opts = AnalyzeOptions {
            find_duplicates: cli.find_duplicates,
            collections: cli.collections,
            collection_config: cli.collection_config.clone(),
            coll_descs: crate::collection_config::load_collection_descs(
                cli.collection_config.as_deref(),
            ),
            ref_paths: cli.ref_paths,
            ..opts
        };
        // Build the MAT index emitter when --mat DIR is set. The prefix is the
        // input basename with a trailing `.hprof[.gz]` stripped, matching how
        // MAT names its cache files (`dump_.hprof` -> `dump_.<kind>.index`).
        let mat = match cli.mat.as_deref() {
            Some(dir) => {
                let base = std::path::Path::new(&input)
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("dump");
                let prefix = base
                    .strip_suffix(".hprof.gz")
                    .or_else(|| base.strip_suffix(".hprof"))
                    .unwrap_or(base);
                match mat::MatEmitter::new(dir, prefix, cli.mat_binary.as_deref()) {
                    Ok(e) => Some(e),
                    Err(e) => fail(format!("cannot create MAT index dir '{}': {e}", dir.display())),
                }
            }
            None => None,
        };
        if let Err(e) = run(
            &input,
            cli.output.as_deref(),
            fmt,
            cli.verbose,
            cvec::Codec::Zstd3,
            opts,
            mat,
        ) {
            fail(analyze_error_hint(&input, &e));
        }
    } else {
        // Re-render path. Analyze-only flags have no effect here — refuse them
        // with a hint rather than silently ignoring them.
        if cli.collections {
            fail(
                "--collections has no effect when re-rendering a saved report; \
                  re-run on the .hprof dump to include it",
            );
        }
        if cli.collection_config.is_some() {
            fail(
                "--collection-config has no effect when re-rendering a saved report; \
                  re-run on the .hprof dump to use it",
            );
        }
        if cli.find_duplicates {
            fail(
                "--find-duplicates has no effect when re-rendering a saved report; \
                  re-run on the .hprof dump to include it",
            );
        }
        if cli.mat.is_some() {
            fail(
                "--mat has no effect when re-rendering a saved report; \
                  re-run on the .hprof dump to emit MAT index files",
            );
        }
        if cli.detail != DetailLevel::Default {
            fail(
                "--detail has no effect when re-rendering a saved report; \
                  re-run on the .hprof dump to change output caps",
            );
        }
        // --verbose / --trace-rss / --progress are analyze-pipeline diagnostics;
        // they are harmless no-ops on the fast re-render path, so we accept them
        // silently rather than refuse them (unlike the data-affecting flags above).
        let fmt = resolve_format(cli.format, cli.output.as_deref());
        match render_report(&input, fmt) {
            Ok(text) => {
                if let Err(e) = write_output(cli.output.as_deref(), &text) {
                    let target = cli.output.as_deref().unwrap_or("<stdout>");
                    fail(format!("cannot write '{target}': {e}"));
                }
            }
            Err(e) => fail(render_error_hint(&input, &e)),
        }
    }
}

/// Decide whether `input` should run the analyze pipeline. True when the path
/// has a `.hprof` / `.hprof.gz` extension OR the file begins with the HPROF
/// magic (`JAVA PROFILE`). `-` (stdin) is never HPROF: a non-seekable pipe of a
/// dump was never supported, and the render path handles `-`.
fn input_is_hprof(input: &str) -> bool {
    if input == "-" {
        return false;
    }
    let lower = input.to_ascii_lowercase();
    if lower.ends_with(".hprof") || lower.ends_with(".hprof.gz") {
        return true;
    }
    looks_like_hprof(input)
}

/// Turn an `analyze` pipeline error into an actionable message. A missing input
/// file is the most common mistake, so name the path explicitly — but only when
/// the error is a bare `NotFound` from opening the input. Output-write failures
/// already carry a `cannot write '…'` message (see `run`), so leave those alone.
/// A file routed here on its `.hprof` extension but lacking the HPROF magic is
/// almost certainly a saved report JSON misnamed as a dump — say so.
fn analyze_error_hint(input: &str, e: &io::Error) -> String {
    let msg = e.to_string();
    if e.kind() == io::ErrorKind::NotFound && !msg.starts_with("cannot ") {
        return format!("cannot open '{input}': no such file or directory");
    }
    if !looks_like_hprof(input) && std::fs::metadata(input).is_ok() {
        return format!(
            "{msg}\n(hint: '{input}' does not start with the HPROF magic; if it \
             is a saved report JSON, rename it without the .hprof extension to \
             re-render it)"
        );
    }
    // A genuine dump (HPROF magic present) that hits EOF mid-record is almost
    // always a truncated or partially-copied file — the terse reader message
    // ("eof in read_into" / "eof in skip") gives no hint of that. Say so.
    if e.kind() == io::ErrorKind::UnexpectedEof && looks_like_hprof(input) {
        return format!(
            "{msg}\n(hint: '{input}' appears truncated or corrupt — the parser \
             hit end of file mid-record; re-copy the .hprof dump and retry)"
        );
    }
    msg
}

/// Turn a `render` error into an actionable message. The most common mistake is
/// feeding the re-render path a *rendered* report (HTML/Markdown) instead of the
/// canonical `.json` it was rendered from; serde then fails with a bare
/// "invalid report JSON" that gives no clue what the file actually is. Sniff the
/// first non-whitespace bytes and name the likely format so the fix is obvious.
fn render_error_hint(input: &str, e: &io::Error) -> String {
    if e.kind() == io::ErrorKind::NotFound {
        return format!("cannot open '{input}': no such file or directory");
    }
    let msg = e.to_string();
    if msg.starts_with("invalid report JSON") && input != "-" {
        match sniff_report_kind(input) {
            Some("html") => {
                return format!(
                    "{msg}\n(hint: '{input}' looks like a rendered HTML report; \
                     re-render from the saved report JSON (.json/.json.gz), not \
                     the .html)"
                );
            }
            Some("markdown") => {
                return format!(
                    "{msg}\n(hint: '{input}' looks like a rendered Markdown report; \
                     re-render from the saved report JSON (.json/.json.gz))"
                );
            }
            _ => {
                return format!(
                    "{msg}\n(hint: expected a saved report JSON (.json/.json.gz); \
                     analyze a .hprof dump to produce one)"
                );
            }
        }
    }
    msg
}

/// Peek at the first non-whitespace bytes of `path` (transparently gunzipping a
/// gzip-magic prefix) to guess whether a non-JSON re-render input is a rendered
/// HTML report (`<`), a Markdown report (`#`/`|`), or unknown. Best-effort: any
/// read error yields `None`, and the caller falls back to a generic hint.
fn sniff_report_kind(path: &str) -> Option<&'static str> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).ok()?;
    let mut head = [0u8; 512];
    let n = f.read(&mut head).ok()?;
    let head = &head[..n];
    // Transparently sniff through a gzip prefix so `.md.gz` / `.html.gz` are
    // classified too (matches how render_report gunzips its input).
    let decoded;
    let bytes: &[u8] = if head.starts_with(&[0x1f, 0x8b]) {
        let mut d = flate2::read::GzDecoder::new(head);
        let mut buf = Vec::new();
        // A short read is fine; we only need the leading bytes.
        let _ = d.read_to_end(&mut buf);
        if buf.is_empty() {
            return None;
        }
        decoded = buf;
        &decoded
    } else {
        head
    };
    let s = String::from_utf8_lossy(bytes);
    let t = s.trim_start();
    let lower = t.to_ascii_lowercase();
    if t.starts_with('<') || lower.contains("<!doctype") || lower.contains("<html") {
        return Some("html");
    }
    if t.starts_with('#') || t.starts_with('|') {
        return Some("markdown");
    }
    None
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
    if report.schema_version > report::SCHEMA_VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "report schema_version {} is newer than supported version {}; refusing to render",
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
    mat: Option<mat::MatEmitter>,
) -> io::Result<()> {
    let t_total = Instant::now();

    let t = Instant::now();
    progress::phase("scanning dump (pass 1)");
    let p1 = pass1::Pass1::run(input, mat.is_some())?;
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
    // Capture MAT class metadata from p1 before it is consumed by Pass2::build.
    let mat_class_meta: Option<mat::MatClassMeta> = if mat.is_some() {
        Some(mat::MatClassMeta::from_pass1(&p1))
    } else {
        None
    };
    // Capture hprof file offsets for o2hprof emission. Compress immediately so
    // the 90 MB Vec<u64> doesn't sit uncompressed through inbound + dominator.
    let mat_hprof_offsets_c: Option<cvec::CompressedU64> = if mat.is_some() {
        Some(cvec::CompressedU64::compress(&p1.hprof_offsets, compress)?)
    } else {
        None
    };
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
    //
    // MAT: emit the `idx` LongIndex (dense id -> object address) here, while the
    // id_map is still live inside the InboundBuilder (addresses are lost once it
    // is compressed just below). Also precompute the histogram-row ->
    // class-object-id inverse table once; it feeds both o2c and the outbound
    // pseudo class element downstream.
    // MAT id-space remapping: addresses are only available now (before compress).
    // We snapshot all g.n addresses so MatIdMap::build can sort reachable ones by
    // address after idom is known (post compute_dominators). The snapshot is a
    // Vec<u64> of length g.n; compress immediately so it doesn't sit uncompressed
    // through the inbound + dominator peak window (~90 MB for 11M objects).
    let (mat_coc_snapshot, mat_addrs_c): (Option<std::collections::HashMap<u32, u32>>, Option<cvec::CompressedU64>) = if let Some(_) = mat {
        let id_map = inbound
            .id_map
            .as_ref()
            .expect("id_map must be live before compress_id_map for MAT idx emit");
        let addrs: Vec<u64> = (0..g.n).map(|i| id_map.addr_at(i)).collect();
        let coc_snap = g.class_obj_class_idx.clone();
        let addrs_c = cvec::CompressedU64::compress(&addrs, compress)?;
        (Some(coc_snap), Some(addrs_c))
    } else {
        (None, None)
    };
    inbound.compress_id_map(compress)?;
    // shallow/class_idx were already compressed inside pass2 (before the
    // fwd_targets alloc) to keep their ~4GB dense forms off the binding peak;
    // shallow_c/class_idx_c hold the blobs, g.shallow/g.class_idx are empty.
    log(verbose, "compress-cold", t.elapsed().as_secs_f64());

    crate::trace::probe("main: after compress_id_map (before rpo_dfs)");
    let t = Instant::now();
    progress::phase("ordering objects (reverse post-order)");
    let rpo = rpo_dfs::rpo_dfs(g.n, &g.gc_root_indices, &g.fwd_offsets, &g.fwd_targets);
    crate::trace::probe("main: after rpo_dfs (before compress parent_pre)");
    log(verbose, "rpo", t.elapsed().as_secs_f64());

    crate::trace::trim();

    // Retained size within the unreachable forest. This is the ONLY point where
    // both the forward CSR (out-edges) and reachability (rpo.dfn) are alive: the
    // forward CSR is moved into the inbound transpose just below. We extract the
    // unreachable subgraph here, run a self-contained dominator + retained pass
    // on it, and keep only a bounded per-class aggregate on `g` for the report
    // phase. shallow/class_idx are streamed out of their compressed blobs (never
    // re-inflated dense), and the node→dense-id map is a binary search over the
    // sorted unreachable-id list (no n-sized array), so this adds no allocation
    // proportional to the whole heap — it is always on.
    {
        let t = Instant::now();
        progress::phase("retained size (unreachable forest)");
        g.unreachable_retained = unreachable_retained::compute_unreachable_retained(
            g.n,
            &rpo.dfn,
            &g.fwd_offsets,
            &g.fwd_targets,
            &shallow_c,
            &class_idx_c,
            g.class_names.len(),
            &g.class_obj_class_idx,
            &g.class_names,
        )?;
        crate::trace::probe("main: after unreachable_retained (fwd CSR + dfn still alive)");
        log(verbose, "unreachable-retained", t.elapsed().as_secs_f64());
        crate::trace::trim();
    }

    // Compress parent_pre (~2 GB dense) between RPO and inbound to reduce
    // the peak during the transpose loop. parent_pre is not needed until
    // compute_dominators; holding it compressed saves ~1.5 GB at inb_flat alloc
    // and at the transpose peak. Decompressed just before compute_dominators.
    let mut rpo = rpo;
    let parent_pre_count = rpo.parent_pre.len();
    let parent_pre_c = if compress != cvec::Codec::None {
        let c = cvec::CompressedU32::compress(&rpo.parent_pre, compress)?;
        rpo.parent_pre = Vec::new();
        Some(c)
    } else {
        None
    };
    crate::trace::probe("main: after compress parent_pre (before inbound)");

    // Build the inbound CSR by transposing the forward CSR, avoiding a third
    // full-file scan. The fwd CSR and rpo.dfn are both still alive here:
    // dfn is needed for node→pre-order translation in Phase 4 of the encode.
    // After the transpose the fwd CSR and id_map (inside InboundBuilder) are
    // freed; vertex is rebuilt once inb_flat encoding is done so it never
    // coexists with the large inb_flat intermediate.
    let t = Instant::now();
    progress::phase("building inbound references");
    // MAT: snapshot the forward CSR and class_idx before build_from_fwd consumes
    // them. We need these to assemble outbound entries in MAT id order after
    // Snapshot fwd_off, fwd_tgt, and class_idx before build_from_fwd consumes
    // fwd_targets. Compress all three immediately; restore each transiently just
    // before use so they don't inflate the inbound + dominator + emit_outbound
    // peak window. class_obj_ids is computed later after mat_inv is built.
    let mat_fwd_snap: Option<(cvec::CompressedU32, cvec::CompressedU32, cvec::CompressedU32)> = if mat.is_some() {
        let class_idx: Vec<u32> = class_idx_c.restore()?;
        let n = g.n;
        let fwd_off_c = cvec::CompressedU32::compress(&g.fwd_offsets, compress)?;
        let total_edges = g.fwd_offsets[n] as usize;
        let mut fwd_tgt: Vec<u32> = Vec::with_capacity(total_edges);
        let mut buf: Vec<u32> = Vec::new();
        for i in 0..n {
            let lo = g.fwd_offsets[i] as usize;
            let hi = g.fwd_offsets[i + 1] as usize;
            if hi > lo {
                let slice: &[u32] = if let Some(sl) = g.fwd_targets.range_slice(lo, hi) {
                    sl
                } else {
                    g.fwd_targets.copy_range(lo, hi, &mut buf);
                    &buf
                };
                fwd_tgt.extend_from_slice(slice);
            }
        }
        let fwd_tgt_c = cvec::CompressedU32::compress(&fwd_tgt, compress)?;
        drop(fwd_tgt);
        let class_idx_c2 = cvec::CompressedU32::compress(&class_idx, compress)?;
        drop(class_idx);
        Some((fwd_off_c, fwd_tgt_c, class_idx_c2))
    } else {
        None
    };
    // fwd_offsets and fwd_targets are moved into build_from_fwd so they can be
    // freed INSIDE the call, before Phase 4 allocates inb_data.
    let (inb_block_off, inb_data) = inbound.build_from_fwd(
        std::mem::take(&mut g.fwd_offsets),
        std::mem::take(&mut g.fwd_targets),
        &rpo.dfn,
    )?;
    log(verbose, "inbound", t.elapsed().as_secs_f64());

    crate::trace::trim();

    // Rebuild vertex: dfn is still live and the inbound encode has returned,
    // so the ~2 GB vertex never coexists with inb_flat. vertex = invert(dfn)
    // is a pure O(n) pass; the dominator reads it next.
    let count = parent_pre_count;
    rpo.vertex = rpo_dfs::rebuild_vertex(&rpo.dfn, count);
    crate::trace::probe("main: after rebuild_vertex (post-inbound, dfn live)");
    // MAT inbound decode needs dfn (dense→pre-order) to build the per-pre-order
    // offset table into inb_data. Save it compressed before clearing; restore
    // just before use so it doesn't inflate the emit_outbound peak (~40 MB win).
    let mat_dfn_save_c: Option<cvec::CompressedU32> = if mat.is_some() {
        let v = std::mem::take(&mut rpo.dfn);
        Some(cvec::CompressedU32::compress(&v, compress)?)
    } else {
        rpo.dfn = Vec::new();
        None
    };
    crate::trace::trim();

    // Restore parent_pre from compressed blob before the dominator stage.
    if let Some(c) = parent_pre_c {
        rpo.parent_pre = c.restore()?;
        crate::trace::probe("main: after restore parent_pre (before dominator)");
    }

    // rpo is consumed by compute_dominators. We already saved dfn above.
    // Also save vertex (pre→dense) compressed so it doesn't inflate the
    // emit_outbound peak (~40 MB win).
    let mat_vertex_save_c: Option<cvec::CompressedU32> = if mat.is_some() {
        Some(cvec::CompressedU32::compress(&rpo.vertex, compress)?)
    } else {
        None
    };

    let t = Instant::now();
    progress::phase("computing dominators");
    // rpo moved by value; vertex/parent_pre owned through translation. dfn
    // already freed above. No separate drop(rpo).
    g.idom =
        dominator::compute_dominators(g.n, rpo, &g.gc_root_indices, &inb_block_off, &inb_data)?;
    log(verbose, "dominator", t.elapsed().as_secs_f64());

    crate::trace::probe("main: after dominator");
    // inb_block_off is only needed by compute_dominators; free it now.
    drop(inb_block_off);
    // inb_data (vbyte-encoded inbound edges, ~87 MB for vscode) is only needed
    // after emit_outbound (for inb_pre_off build + emit_inbound). Compress it
    // across the emit_outbound peak window to free ~80 MB.
    let inb_data_c: cvec::CompressedBytes = if mat.is_some() {
        cvec::CompressedBytes::compress(inb_data, compress)?
    } else {
        cvec::CompressedBytes::compress(inb_data, cvec::Codec::None)?
    };
    crate::trace::trim();
    // MAT id-space remapping: now that idom is set we can build the mapping from
    // our dense-id space to MAT's (reachable-only, address-sorted, id-0=synthetic).
    // Restore mat_addrs_c here (just before use) so it's uncompressed for the
    // minimum possible window.
    let mat_map: Option<mat::MatIdMap> = if let Some(addrs_c) = mat_addrs_c {
        let addrs = addrs_c.restore()?;
        let mm = mat::MatIdMap::build(g.n, &g.idom, |i| addrs[i]);
        // emit idx: mat-id 0 = address 0x0 (synthetic root), then sorted reachable
        if let Some(ref m) = mat {
            let mc = mm.mat_count();
            let mut idx_vals: Vec<i64> = Vec::with_capacity(mc);
            idx_vals.push(0i64); // synthetic root at address 0x0
            for &old_id in mm.sorted() {
                idx_vals.push(addrs[old_id as usize] as i64);
            }
            m.emit_long_index("idx", &idx_vals)?;
            // o2hprof: restore offsets here (just before use), emit, drop.
            if let Some(ref off_c) = mat_hprof_offsets_c {
                let offsets = off_c.restore()?;
                let mut o2hprof_vals: Vec<i64> = Vec::with_capacity(mc + 1);
                o2hprof_vals.push(0i64);
                for &old_id in mm.sorted() {
                    o2hprof_vals.push(offsets[old_id as usize] as i64);
                }
                m.emit_long_index("o2hprof", &o2hprof_vals)?;
                drop(o2hprof_vals);
                drop(offsets);
            }
        }
        drop(addrs);
        Some(mm)
    } else {
        None
    };
    drop(mat_hprof_offsets_c);
    // Build the row→class-object id inverse table now that mm is available, so
    // we can prefer reachable class-objects when multiple map to the same row.
    let mut mat_inv: Option<Vec<i32>> = if let (Some(ref mm), Some(ref coc)) =
        (mat_map.as_ref(), mat_coc_snapshot.as_ref())
    {
        Some(mat::build_row_to_classobj_id(coc, g.class_names.len(), mm))
    } else {
        None
    };
    // Patch alias rows: some class names have two histogram rows — a canonical row
    // (JLC_KEY or PRIM_KEY) where the class-object is registered, and an addr-based
    // row where instances are counted. Only the canonical row has inv[row] != -1.
    // For the MAT o2c table, alias rows (same name, inv==-1) must map to the same
    // class-object as the canonical row.
    if let (Some(ref mut inv), Some(ref mm)) = (mat_inv.as_mut(), mat_map.as_ref()) {
        // Build: name → canonical class-object mat-id (first row with inv[row]!=-1)
        let mut name_to_coid: std::collections::HashMap<&str, i32> = std::collections::HashMap::new();
        for (row, name) in g.class_names.iter().enumerate() {
            if inv[row] >= 0 {
                name_to_coid.entry(name.as_str()).or_insert(inv[row]);
            }
        }
        // Fill alias rows that have inv==-1 but a canonical entry for the same name exists.
        for (row, name) in g.class_names.iter().enumerate() {
            if inv[row] < 0 {
                if let Some(&coid) = name_to_coid.get(name.as_str()) {
                    inv[row] = coid;
                }
            }
        }
        let _ = mm;  // mm borrow ends here
    };
    // Resolve class_obj_ids from the raw class_idx rows now that mat_inv is ready.
    // mat_fwd_snap.2 holds the compressed class_idx; restore, map through inv,
    // then re-compress so the 45 MB array doesn't inflate the emit_outbound peak.
    let mat_class_obj_ids_c: Option<cvec::CompressedU32> =
        if let (Some(ref inv), Some(ref fwd_snap)) = (mat_inv.as_ref(), mat_fwd_snap.as_ref()) {
            let class_idx_rows = fwd_snap.2.restore()?;
            let result: Vec<u32> = class_idx_rows
                .iter()
                .map(|&row| {
                    let coid = inv[row as usize];
                    if coid < 0 { u32::MAX } else { coid as u32 }
                })
                .collect();
            Some(cvec::CompressedU32::compress(&result, compress)?)
        } else {
            None
        };
    crate::trace::probe("main: before emit_outbound");
    // MAT: emit `outbound` IntArray1N in MAT id order. Restore the compressed
    // fwd_off, fwd_tgt, and class_obj_ids here (just before use) to minimise
    // the window they occupy uncompressed.
    if let Some(ref m) = mat {
        let mm = mat_map.as_ref().expect("mat_map built with mat");
        if let Some((fwd_off_c, fwd_tgt_c, _class_idx_c)) = mat_fwd_snap.as_ref() {
            let class_obj_ids = mat_class_obj_ids_c
                .as_ref()
                .expect("mat_class_obj_ids_c built when mat present")
                .restore()?;
            let fwd_off = fwd_off_c.restore()?;
            let fwd_tgt = fwd_tgt_c.restore()?;
            let n_entries = mm.mat_count(); // includes synthetic root at idx 0
            let sorted = mm.sorted();
            let mut idx = 0usize; // index into sorted (0 = synthetic root)
            let mut scratch: Vec<i32> = Vec::new();
            m.emit_outbound_cb(n_entries, |push| {
                if idx == 0 {
                    // synthetic root: no outbound edges
                    idx += 1;
                    return Ok(());
                }
                let old_id = sorted[idx - 1];
                idx += 1;
                let lo = fwd_off[old_id as usize] as usize;
                let hi = fwd_off[old_id as usize + 1] as usize;
                scratch.clear();
                for &raw in &fwd_tgt[lo..hi] {
                    let mid = mm.translate(raw as i32);
                    if mid >= 0 {
                        scratch.push(mid);
                    }
                }
                scratch.sort_unstable();
                scratch.dedup();
                let coid = class_obj_ids[old_id as usize];
                let class_mat = if coid == u32::MAX { 0 } else { mm.translate(coid as i32).max(0) };
                if let Ok(pos) = scratch.binary_search(&class_mat) {
                    scratch.remove(pos);
                }
                // push class_mat first, then remaining targets (already sorted)
                push(class_mat)?;
                for &v in &scratch {
                    push(v)?;
                }
                Ok(())
            })?;
            drop(fwd_tgt);
            drop(fwd_off);
            drop(class_obj_ids);
            crate::trace::trim();
        }
    }
    drop(mat_fwd_snap);
    crate::trace::probe("main: after drop(mat_fwd_snap) — restore inb_data + build inb offset table");
    // Restore inb_data now (was compressed across emit_outbound to save ~80 MB).
    let inb_data = inb_data_c.restore()?;
    // MAT inbound: build a per-pre-order byte-offset table into inb_data (~44 MB
    // for 11M objects) so we can decode each object's referrers on demand rather
    // than materialising the full Vec<Vec<i32>> (~550 MB).
    // dfn[dense] = pre_order; inb_pre_off[pre_order] = byte offset in inb_data;
    // vertex[pre_order] = dense_id (needed to decode referrer pre-order → dense-id).
    let mat_inb_ctx: Option<(Vec<u32>, Vec<u32>, Vec<u32>)> =
        if let (Some(dfn_c), Some(vertex_c)) = (mat_dfn_save_c, mat_vertex_save_c) {
            let dfn = dfn_c.restore()?;
            let vertex = vertex_c.restore()?;
            let n = g.n;
            let mut off: Vec<u32> = Vec::with_capacity(n);
            let mut pos = 0usize;
            for _pre in 0..n {
                off.push(pos as u32);
                let (count, c0) = vbyte::decode_one(&inb_data[pos..]);
                pos += c0;
                for _ in 0..count {
                    let (_, c1) = vbyte::decode_one(&inb_data[pos..]);
                    pos += c1;
                }
            }
            crate::trace::probe("main: after inb_pre_off build");
            Some((dfn, off, vertex))
        } else {
            None
        };
    // MAT: emit `inbound` IntArray1N in MAT id order. Decode each object's
    // referrers on demand from inb_data using the offset table.
    if let Some(ref m) = mat {
        let mm = mat_map.as_ref().expect("mat_map built with mat");
        if let Some((dfn, inb_pre_off, vertex)) = mat_inb_ctx.as_ref() {
            let iter = std::iter::once(Vec::new()) // entry 0 = synthetic root
                .chain(mm.sorted().iter().map(|&old_id| {
                    let pre = dfn[old_id as usize] as usize;
                    let mut pos = inb_pre_off[pre] as usize;
                    let (count, c0) = vbyte::decode_one(&inb_data[pos..]);
                    pos += c0;
                    let mut e: Vec<i32> = Vec::with_capacity(count as usize);
                    let mut prev: u32 = 0;
                    for _ in 0..count {
                        let (delta, c1) = vbyte::decode_one(&inb_data[pos..]);
                        pos += c1;
                        prev += delta;
                        if prev > 0 {
                            let dense = vertex[prev as usize] as i32;
                            let mid = mm.translate(dense);
                            if mid >= 0 { e.push(mid); }
                        }
                    }
                    e.sort_unstable();
                    e
                }));
            m.emit_inbound_iter(iter)?;
            crate::trace::trim();
        }
    }
    drop(mat_inb_ctx);
    // MAT: emit the `domIn` IntIndex in MAT id order. mat-id 0 (synthetic root)
    // has no dominator (superroot = -1, stored as 1). For mat-ids 1..N look up
    // the old dense-id, translate idom[old] to a mat-id, then add 2 per MAT's
    // convention (superroot stored as 1, id 0 → 2, etc.).
    if let Some(ref m) = mat {
        let mm = mat_map.as_ref().expect("mat_map built with mat");
        let n_u = g.n as u32;
        let mc = mm.mat_count();
        let mut domin: Vec<i32> = Vec::with_capacity(mc);
        // id-0 = synthetic superroot, its own dominator is the superroot (-1+2=1)
        domin.push(1i32);
        for &old_id in mm.sorted() {
            let d = g.idom[old_id as usize];
            let mat_idom = if d == n_u || d == u32::MAX {
                // dominated by virtual root = MAT superroot (-1), stored +2 = 1
                1i32
            } else {
                // translate old idom to mat-id (+2 per MAT convention)
                let mid = mm.translate(d as i32);
                if mid < 0 { 1i32 } else { mid + 2 }
            };
            domin.push(mat_idom);
        }
        m.emit_dom_in(&domin)?;
    }
    // inb_data is consumed by emit_inbound (for MAT) and then no longer needed.
    // inb_block_off was already freed right after compute_dominators above.
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

    // MAT: emit the `domOut` IntArray1N (unsorted) in MAT id order.
    // Layout: entry[0] = vroot's dom-children (= MAT GC roots), entry[1] = dom-
    // children of mat-id 0 (synthetic root, always empty), entries[2..mc+1] =
    // dom-children of mat-ids 1..mc-1 (real objects). Streaming to avoid a
    // large Vec<Vec<i32>> materialisation.
    if let Some(ref m) = mat {
        let mm = mat_map.as_ref().expect("mat_map built with mat");
        let n = g.n;
        // entry 0: vroot's dom-children
        let lo0 = dc_off[n] as usize;
        let hi0 = dc_off[n + 1] as usize;
        let vroot_children: Vec<i32> = dc_tgt[lo0..hi0]
            .iter()
            .filter_map(|&v| { let mid = mm.translate(v as i32); if mid >= 0 { Some(mid) } else { None } })
            .collect();
        let iter = std::iter::once(vroot_children)
            .chain(std::iter::once(Vec::new())) // entry 1: mat-id 0 synthetic root
            .chain(mm.sorted().iter().map(|&old_id| {
                let lo = dc_off[old_id as usize] as usize;
                let hi = dc_off[old_id as usize + 1] as usize;
                dc_tgt[lo..hi]
                    .iter()
                    .filter_map(|&v| { let mid = mm.translate(v as i32); if mid >= 0 { Some(mid) } else { None } })
                    .collect::<Vec<i32>>()
            }));
        m.emit_dom_out_iter(iter)?;
        crate::trace::trim();
    }

    // MAT: emit o2c (mat-id -> class-object mat-id) and a2s (mat-id -> shallow
    // size) in MAT id order. Restore the compressed blobs transiently for random
    // access by old dense-id; the restore below will do it again for the report
    // phase (restoring a compressed blob is idempotent and cheap).
    if let Some(ref m) = mat {
        let mm = mat_map.as_ref().expect("mat_map built with mat");
        let inv = mat_inv.as_ref().expect("mat_inv built when mat present");
        let class_idx_vec: Vec<u32> = class_idx_c.restore()?;
        let shallow_vec: Vec<u32> = shallow_c.restore()?;
        let mc = mm.mat_count();

        // o2c: mat-id 0 (synthetic root) → class-id 0
        let mut o2c_vals: Vec<i32> = Vec::with_capacity(mc);
        o2c_vals.push(0i32);
        for &old_id in mm.sorted() {
            let row = class_idx_vec[old_id as usize];
            let class_obj_old = inv[row as usize]; // old dense class-object id (-1 = missing)
            let class_obj_mat = mm.translate(class_obj_old);
            o2c_vals.push(if class_obj_mat >= 0 { class_obj_mat } else { 0 });
        }
        drop(class_idx_vec);
        m.emit_int_index("o2c", &o2c_vals)?;
        drop(o2c_vals);

        // a2s: mat-id 0 → size 0; others from shallow in old-id order
        let mut a2s_vals: Vec<i32> = Vec::with_capacity(mc);
        a2s_vals.push(0i32);
        for &old_id in mm.sorted() {
            let sz = shallow_vec[old_id as usize] as i64;
            a2s_vals.push(mat::size_compress(sz));
        }
        drop(shallow_vec);
        m.emit_int_index("a2s", &a2s_vals)?;
    }

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

    // MAT: emit the `o2ret` LongIndex in MAT id order.
    // mat-id 0 (synthetic root) gets 0 retained size.
    if let Some(ref m) = mat {
        let mm = mat_map.as_ref().expect("mat_map built with mat");
        let mc = mm.mat_count();
        let mut o2ret_vals: Vec<i64> = Vec::with_capacity(mc);
        o2ret_vals.push(0i64); // synthetic root
        for &old_id in mm.sorted() {
            o2ret_vals.push(g.retained[old_id as usize] as i64);
        }
        m.emit_long_index("o2ret", &o2ret_vals)?;
    }

    // MAT: emit i2sv2 (per-class retained-size cache) and .threads.
    // Both depend on g.retained (just computed above) and mm (mat id map).
    if let Some(ref m) = mat {
        let mm = mat_map.as_ref().expect("mat_map built with mat");
        let inv = mat_inv.as_ref().expect("mat_inv built when mat present");

        // i2sv2: sum retained sizes by class (in mat-id order of the class object).
        // inv[row] = old_class_obj_dense_id; mm.translate → class mat-id.
        // Accumulate per-row retained sums, then emit (class_mat_id, sum) pairs.
        let num_rows = g.class_names.len();
        let mut per_class_retained: Vec<i64> = vec![0i64; num_rows];
        for i in 0..g.n {
            if g.idom[i] != u32::MAX {
                let row = g.class_idx[i] as usize;
                if row < num_rows {
                    per_class_retained[row] += g.retained[i] as i64;
                }
            }
        }
        let class_iter = (0..num_rows).filter_map(|row| {
            let old_cobj = inv[row];
            if old_cobj < 0 { return None; }
            let mat_cid = mm.translate(old_cobj);
            if mat_cid <= 0 { return None; }
            Some((mat_cid, per_class_retained[row]))
        });
        m.emit_i2sv2(class_iter)?;

        // .threads: thread stacks (addresses from mm).
        m.emit_threads(&g.thread_stacks, mm, &g.thread_local_frame_samples)?;

        // .index: master Java serialization stream.
        if let Some(ref meta) = mat_class_meta {
            m.emit_dot_index(
                meta,
                &g.class_names,
                &g.class_loader_id,
                &g.class_obj_class_idx,
                inv,
                mm,
                g.n,
                &g.shallow,
                &g.class_idx,
            )?;
        }
    }

    // Restore + aggregate + free the alloc stack serials in a bounded window
    // right after compute_retained (needs g.shallow + g.retained, both live
    // now). Skipped entirely when skip_report is set (mat caches mode).
    let alloc_sites = if opts.skip_report {
        None
    } else if let Some(c) = alloc_serial_c {
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

    if opts.skip_report {
        // mat caches mode: no report needed, skip build_model + render entirely.
        drop(dc_off);
        drop(dc_tgt);
        log(verbose, "total", t_total.elapsed().as_secs_f64());
        return Ok(());
    }

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
    write_output(output, &out_text).map_err(|e| {
        // Name the OUTPUT path here so the analyze error hint does not later
        // re-attribute an output-write failure to the input file.
        let target = output.unwrap_or("<stdout>");
        io::Error::new(e.kind(), format!("cannot write '{target}': {e}"))
    })?;

    log(verbose, "total", t_total.elapsed().as_secs_f64());
    Ok(())
}

/// Emit pass-1 parse stats (counts + class histogram) as JSON to stdout.
fn dump_pass1_json(path: &str) -> io::Result<()> {
    let p = Pass1::run(path, false)?;

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

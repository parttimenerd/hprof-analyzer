//! CLI entry point and two-pass orchestration for the HPROF heap-dump analyzer.
//!
//! The default (no-subcommand) form sniffs the positional input: a `.hprof`,
//! `.hprof.gz`, `.tar.gz`, or `.tgz` dump (or HPROF magic) runs the analyze
//! pipeline, anything else is re-rendered as a saved Report JSON. Named
//! subcommands: `compare mat` (MAT export vs our
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
mod named_queries;
mod opts;
mod pass1;
mod pass2;
mod progress;
mod query;
mod reader;
mod report;
mod retained;
mod rpo_dfs;
mod run_oql;
mod serve;
mod source;
mod sweep;
mod trace;
mod types;
mod unreachable_retained;
mod update;
mod vbyte;

use std::io::IsTerminal;
use std::{io, process, time::Instant};

use opts::{AnalyzeOptions, DEFAULT_QUERY_PATH_DEPTH, DetailLevel, OutputFormat};
use pass1::Pass1;
use run_oql::{NoClassIndex, RetainedEdgeStructs, query_uses_edges, run_oql_escalated};

/// clap `value_parser` for `--query-path-depth`: reject `0` (and non-numeric
/// input) with an actionable message. `usize`'s parser already rejects negative
/// and non-numeric values; we add the `> 0` guard on top.
fn parse_query_path_depth(s: &str) -> Result<usize, String> {
    let n: usize = s
        .parse()
        .map_err(|_| format!("`{s}` is not a valid non-negative integer for --query-path-depth"))?;
    if n == 0 {
        return Err(
            "--query-path-depth must be > 0 (bounded path walks need at least one hop)".into(),
        );
    }
    Ok(n)
}

use clap::{CommandFactory, Parser, Subcommand, ValueEnum, ValueHint};
use clap_complete::Shell;

/// Analyze a heap dump or re-render a saved report. The input is sniffed:
/// a `.hprof`, `.hprof.gz`, `.tar.gz`, or `.tgz` dump (or any file starting
/// with the HPROF magic) runs the full analysis pipeline; anything else is
/// treated as a saved Report JSON and re-rendered.
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
        hprof-analyzer query heap.hprof --query 'SELECT COUNT(*) FROM java.lang.String'  # ad-hoc OQL\n  \
        hprof-analyzer query heap.hprof --repl            # interactive OQL shell\n  \
        hprof-analyzer heap.hprof out.html --query-file q.oql       # queries folded into a report\n  \
        hprof-analyzer compare reports r1.json r2.json [r3.json …]  # cross-dump growth diff\n  \
        hprof-analyzer completions zsh > _hprof-analyzer  # shell completions\n\n\
        OQL grammar, the -- @viz chart directive, and the --query= equals-form\n  \
        gotcha are documented in docs/OQL.md.\n\n\
        Install zsh completions:\n  \
        hprof-analyzer completions zsh > \"${fpath[1]}/_hprof-analyzer\"",
    args_conflicts_with_subcommands = true
)]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,

    /// A `.hprof`, `.hprof.gz`, `.hprof.tar.gz`, `.tar.gz`, or `.tgz` heap dump
    /// to analyze, or a saved Report JSON (or `.json.gz`, or `-` for stdin) to
    /// re-render. Required when no subcommand is given.
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

    /// Print resident-set size (RSS) at each labeled pipeline stage to stderr,
    /// as `[trace-rss] <stage> RSS=N MB (peak N MB)`. Linux reads VmHWM for the
    /// peak; use it to pinpoint which stage drives the memory high-water mark.
    /// Analyze-only.
    #[arg(long)]
    trace_rss: bool,

    /// Show a live progress line on stderr. `auto` (default) enables it only
    /// when stderr is a terminal and --verbose is not set.
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

    /// Run an OQL query against the heap and include results in the report.
    /// May be repeated. Example: --query "SELECT * FROM java.lang.String".
    /// To embed a `-- @viz` chart directive, use the attached equals form so
    /// the leading `--` is not read as a flag: --query="-- @viz histogram
    /// <newline> SELECT ...". See docs/OQL.md for the full grammar.
    #[arg(long = "query", value_name = "OQL")]
    query: Vec<String>,

    /// Read one OQL query per non-empty line from a file (lines starting with
    /// `#` are comments; a `-- @viz` line attaches to the next query).
    #[arg(long = "query-file", value_name = "PATH", value_hint = ValueHint::FilePath)]
    query_file: Option<String>,

    /// Max hops for OQL `path(a, b)` bounded walks (must be > 0).
    #[arg(long = "query-path-depth", value_name = "N", default_value_t = DEFAULT_QUERY_PATH_DEPTH, value_parser = parse_query_path_depth)]
    query_path_depth: usize,

    /// Restrict OQL results to GC-reachable objects (Eclipse MAT parity). Off by
    /// default for analyze so the report stays byte-identical; opt in to prune
    /// unreachable objects from `--query` results.
    #[arg(long)]
    reachable_only: bool,

    /// Store field-name labels on forward edges so that leak-suspect
    /// "path to GC roots" steps show `ParentClass.fieldName → ChildClass`.
    /// Gated (off by default): adds ~2 bytes per reference edge in the heap
    /// (~100–500 MB extra RSS for large dumps). Analyze-only.
    #[arg(long)]
    ref_paths: bool,

    /// Collect per-class reference-field statistics: for the top-50 most
    /// common classes by instance count, report total non-null ref counts and
    /// sum of retained sizes for their pointees. Adds one O(n) pass.
    #[arg(long)]
    field_stats: bool,

    /// How many histogram rows (sorted by retained desc) get an expandable
    /// GC-root path in the report. Default 20; use 0 to disable. Overrides
    /// the value set by --detail. Analyze-only.
    #[arg(long, value_name = "N")]
    hist_root_path_top: Option<usize>,

    /// Enable all heavy opt-in analyses: equivalent to passing
    /// --obj-graph --collections --find-duplicates together.
    /// Adds ~330 MB peak RSS on large dumps (--obj-graph ~30 MB,
    /// --collections ~300 MB). --ref-paths is excluded because it can add
    /// 100–500 MB on top; pass it separately when field-name labels are needed.
    /// Analyze-only.
    #[arg(long)]
    full_analysis: bool,

    /// Capture the outbound-reference graph and dominator subtree for the top
    /// retained objects. Enables the interactive Object Graph Explorer (outbound
    /// refs, inbound refs, GC-root path, dominator drill-down) in the HTML report.
    /// Adds ~1–3 MB to the report; ~30 MB peak RSS freed after analysis.
    /// Implied by --full-analysis. Analyze-only.
    ///
    /// Optional value controls the edge capture tier:
    ///   --obj-graph          → small  (100 edges/obj, ~1–3 MB delta)
    ///   --obj-graph=medium   → medium (150 edges/obj, ~2–5 MB delta)
    ///   --obj-graph=large    → large  (300 edges/obj, ~5–15 MB delta)
    #[arg(long, num_args = 0..=1, default_missing_value = "small", value_name = "TIER")]
    obj_graph: Option<String>,

    /// Collection-detail size preset for --collections / --full-analysis.
    /// Controls how many holder-edges, container records, node-KV entries, and
    /// element-type samples are captured. Larger sizes give richer collection
    /// breakdowns at the cost of higher peak RSS.
    ///
    ///   --size small   → lowest RSS (1M edges, 10k collections tracked)
    ///   --size default → balanced (2.5M edges, 50k collections) [default]
    ///   --size large   → 2× balanced caps (5M edges, 100k collections)
    ///   --size max     → original uncapped limits (10M edges, 200k collections)
    ///
    /// Only affects --collections / --full-analysis; ignored for basic analysis.
    #[arg(long, default_value = "default", value_name = "SIZE")]
    size: String,

    /// Embed the React app bundle as plain readable JS in the HTML report
    /// (no deflate/base64). The report is ~750 KB larger but the JS is visible
    /// and editable in browser DevTools — useful for iterating on the UI.
    /// Implies --format html. Analyze-only.
    #[arg(long)]
    dev: bool,

    /// Read the React app bundle from PATH instead of the compile-time embedded
    /// bytes. Use together with --dev to iterate on JS/CSS without rebuilding
    /// the binary: run `node esbuild.config.mjs` in web/, then re-run this
    /// command with --dev --bundle-path web/dist/bundle.js.
    /// Implies --dev.
    #[arg(long, value_name = "PATH", value_hint = ValueHint::FilePath)]
    bundle_path: Option<std::path::PathBuf>,

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
    /// Show available versions and optionally replace the running binary.
    ///
    /// With no argument: shows the current version, latest nightly build, and
    /// latest stable release without changing anything.
    ///
    /// With a channel argument: downloads that release and replaces this binary.
    ///   hprof-analyzer update nightly   # replace with latest nightly
    ///   hprof-analyzer update latest    # replace with latest stable release
    Update {
        /// Channel to update from. Omit to just show version info.
        #[arg(value_enum)]
        channel: Option<update::Channel>,
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
    /// Start a loopback HTTP server exposing OQL and report-section endpoints.
    Server {
        /// Path to the .hprof dump (.hprof, .hprof.gz, .tar.gz, .tgz).
        #[arg(value_hint = ValueHint::FilePath)]
        input: String,
        /// Port to bind (default 7070; loopback only).
        #[arg(long, value_name = "N")]
        port: Option<u16>,
    },
    /// Run one or more OQL queries against a heap dump and print the results.
    /// Fast query-only path (no full report): retained-size, dominator, and
    /// reference-graph attributes (@retainedHeapSize, dominators(x), @inbounds,
    /// path(a,b), ...) need the full report instead. See docs/OQL.md.
    Query {
        /// Path to the .hprof dump (.hprof, .hprof.gz, .hprof.zip, .tar.gz, .tgz).
        #[arg(value_hint = ValueHint::FilePath)]
        input: String,
        /// OQL query text (may be repeated). For a `-- @viz` directive use the
        /// attached equals form: --query="-- @viz histogram\nSELECT ...".
        #[arg(long = "query", value_name = "OQL")]
        query: Vec<String>,
        /// Read queries from a file, one per line (`#` comments allowed; a
        /// `-- @viz` line attaches to the next query).
        #[arg(long = "query-file", value_name = "PATH", value_hint = ValueHint::FilePath)]
        query_file: Option<String>,
        /// Max hops for OQL `path(a, b)` bounded walks (must be > 0).
        #[arg(long = "query-path-depth", value_name = "N", default_value_t = DEFAULT_QUERY_PATH_DEPTH, value_parser = parse_query_path_depth)]
        query_path_depth: usize,
        /// Start an interactive OQL REPL reading queries from stdin.
        #[arg(long)]
        repl: bool,
        /// Run a loopback HTTP server so tools can POST OQL and get JSON back.
        /// See the startup banner for usage; GET /help returns the language ref.
        #[arg(long, conflicts_with = "repl")]
        server: bool,
        /// Port for --server (default 7070; binds 127.0.0.1 only).
        #[arg(long, value_name = "N")]
        port: Option<u16>,
        /// Restrict OQL results to GC-reachable objects (Eclipse MAT parity).
        /// This is the default for the `query` subcommand; pass `--all` to
        /// include unreachable objects (raw heap scan).
        #[arg(long, conflicts_with = "all")]
        reachable_only: bool,
        /// Include unreachable objects in OQL results (raw heap scan), opting
        /// out of the reachable-only default.
        #[arg(long)]
        all: bool,
        /// Run a named query by name (see `query --list-named` for available names).
        #[arg(long = "run", value_name = "NAME")]
        run: Option<String>,
        /// List all named queries and exit.
        #[arg(long = "list-named")]
        list_named: bool,
        /// Output format for query results.
        /// `text` (default) prints aligned ASCII tables; `json` emits a JSON
        /// array of QueryResult objects suitable for scripting.
        #[arg(short, long, value_enum, default_value_t = QueryFormatArg::Text)]
        format: QueryFormatArg,
        /// When to show the live progress line on stderr.
        #[arg(long, value_enum, default_value_t = ProgressWhen::Auto)]
        progress: ProgressWhen,
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
        /// The `.hprof`, `.hprof.gz`, `.tar.gz`, or `.tgz` heap dump to analyze.
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

/// Output format for the `query` subcommand.
#[derive(Clone, Copy, PartialEq, ValueEnum)]
enum QueryFormatArg {
    /// Aligned ASCII tables (default).
    Text,
    /// JSON array of QueryResult objects.
    Json,
}

/// Output-size preset. `Default` reproduces the historical cap values so
/// MAT/golden parity is unchanged; `Minimal`/`Max` scale the caps down/up.
/// Implement clap's `ValueEnum` manually (the type is defined in `opts.rs`
/// without clap as a dependency, so the derive is not available there).
impl ValueEnum for DetailLevel {
    fn value_variants<'a>() -> &'a [Self] {
        &[DetailLevel::Minimal, DetailLevel::Default, DetailLevel::Max]
    }
    fn to_possible_value(&self) -> Option<clap::builder::PossibleValue> {
        Some(match self {
            DetailLevel::Minimal => clap::builder::PossibleValue::new("minimal"),
            DetailLevel::Default => clap::builder::PossibleValue::new("default"),
            DetailLevel::Max => clap::builder::PossibleValue::new("max"),
        })
    }
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
        Some("-") | None => {
            print!("{text}");
            Ok(())
        }
        Some(p) => std::fs::write(p, text).map_err(|e| io::Error::new(e.kind(), e)),
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
        Some(Cmd::Update { channel }) => {
            if let Err(e) = update::run(channel) {
                fail(e);
            }
        }
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
            CompareCmd::Reports {
                reports,
                format,
                output,
            } => {
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
                                        let mut gz = flate2::write::GzEncoder::new(
                                            f,
                                            flate2::Compression::default(),
                                        );
                                        if let Err(e) = gz
                                            .write_all(&bytes)
                                            .and_then(|_| gz.finish().map(|_| ()))
                                        {
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
        Some(Cmd::Server { input, port }) => {
            if !input_is_hprof(&input) {
                fail(format!(
                    "'{input}' is not an HPROF dump; the `server` subcommand needs a .hprof[.gz/.zip/.tar.gz] file"
                ));
            }
            let opts = AnalyzeOptions {
                reachable_only: true,
                ..DetailLevel::Default.options()
            };
            if let Err(e) = serve::run_server(&input, port.unwrap_or(serve::DEFAULT_PORT), opts) {
                fail(analyze_error_hint(&input, &e));
            }
        }
        Some(Cmd::Query {
            input,
            query,
            query_file,
            query_path_depth,
            repl,
            server,
            port,
            reachable_only: _,
            all,
            run,
            list_named,
            format,
            progress,
        }) => {
            if !input_is_hprof(&input) {
                fail(format!(
                    "'{input}' is not an HPROF dump; the `query` subcommand needs a .hprof[.gz/.zip/.tar.gz] file"
                ));
            }
            // --server and --repl take their queries from HTTP/stdin, so any
            // --query/--query-file is dropped. Warn rather than silently ignore.
            if (server || repl) && (!query.is_empty() || query_file.is_some()) {
                let mode = if server { "--server" } else { "--repl" };
                eprintln!(
                    "warning: {mode} takes queries interactively; --query/--query-file are ignored"
                );
            }
            if server {
                // Loopback HTTP server: POST OQL, get JSON back. Reads no stdin;
                // --query/--query-file are ignored.
                if let Err(e) = crate::query::server::run_server(
                    &input,
                    query_path_depth,
                    port.unwrap_or(serve::DEFAULT_PORT),
                ) {
                    fail(analyze_error_hint(&input, &e));
                }
            } else if repl {
                // Interactive mode reads queries from stdin; --query/--query-file
                // are ignored.
                if let Err(e) = crate::query::repl::run_repl(&input, query_path_depth) {
                    fail(analyze_error_hint(&input, &e));
                }
            } else {
                if list_named {
                    for nq in crate::named_queries::NAMED_QUERIES {
                        println!("{:40}  [{}]  {}", nq.name, nq.group, nq.display);
                    }
                    return;
                }
                let mut queries_vec = query;
                if let Some(ref name) = run {
                    let nq = crate::named_queries::NAMED_QUERIES
                        .iter()
                        .find(|q| q.name == name);
                    match nq {
                        None => {
                            let prefix_end = name
                                .char_indices()
                                .nth(3)
                                .map(|(i, _)| i)
                                .unwrap_or(name.len());
                            let prefix = &name[..prefix_end];
                            let candidates: Vec<&str> = crate::named_queries::NAMED_QUERIES
                                .iter()
                                .filter(|q| q.name.starts_with(prefix))
                                .map(|q| q.name)
                                .collect();
                            eprintln!("error: unknown named query {:?}", name);
                            if !candidates.is_empty() {
                                eprintln!("  did you mean: {}", candidates.join(", "));
                            }
                            std::process::exit(1);
                        }
                        Some(nq) => {
                            queries_vec.push(nq.oql.to_string());
                        }
                    }
                }
                let opts = AnalyzeOptions {
                    queries: queries_vec,
                    query_file,
                    query_path_depth,
                    // Query subcommand defaults to reachable-only (MAT parity);
                    // --all opts back into a raw-heap scan. --reachable-only is
                    // redundant-but-allowed since the default is already true.
                    reachable_only: !all,
                    ..DetailLevel::Default.options()
                };
                // Reuse the analyze pipeline, printing only the query results as text.
                let show_progress = match progress {
                    ProgressWhen::Always => true,
                    ProgressWhen::Never => false,
                    ProgressWhen::Auto => std::io::stderr().is_terminal(),
                };
                progress::set_enabled(show_progress);
                let json_out = format == QueryFormatArg::Json;
                if let Err(e) = run_queries(&input, opts, json_out) {
                    fail(analyze_error_hint(&input, &e));
                }
            }
        }
        Some(Cmd::Mat { cmd }) => match cmd {
            MatCmd::Caches {
                input,
                dir,
                mat_binary,
            } => {
                if !input_is_hprof(&input) {
                    fail(format!(
                        "'{input}' does not look like a .hprof[.gz/.zip/.tar.gz] file"
                    ));
                }
                progress::set_enabled(std::io::stderr().is_terminal());
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
                    .strip_suffix(".hprof.tar.gz")
                    .or_else(|| base.strip_suffix(".hprof.gz"))
                    .or_else(|| base.strip_suffix(".tar.gz"))
                    .or_else(|| base.strip_suffix(".tgz"))
                    .or_else(|| base.strip_suffix(".hprof"))
                    .unwrap_or(base);
                let mat_bin_path = mat_binary.as_deref().map(std::path::Path::new);
                let mat_emitter =
                    match mat::MatEmitter::new(std::path::Path::new(mat_dir), prefix, mat_bin_path)
                    {
                        Ok(e) => e,
                        Err(e) => fail(format!("cannot create MAT index dir '{mat_dir}': {e}")),
                    };
                if let Err(e) = run(
                    &input,
                    Some("/dev/null"),
                    OutputFormat::Md,
                    false,
                    cvec::Codec::Deflate9,
                    AnalyzeOptions {
                        skip_report: true,
                        ..DetailLevel::Default.options()
                    },
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
        let show_progress = match cli.progress {
            ProgressWhen::Always => true,
            ProgressWhen::Never => false,
            ProgressWhen::Auto => !cli.verbose && std::io::stderr().is_terminal(),
        };
        progress::set_enabled(show_progress);
        trace::set_enabled(cli.trace_rss);
        let fmt = if cli.dev || cli.bundle_path.is_some() {
            // --dev implies HTML unless an explicit format or .html extension already means HTML.
            let base = resolve_format(cli.format, cli.output.as_deref());
            if base != OutputFormat::Html {
                OutputFormat::Html
            } else {
                base
            }
        } else {
            resolve_format(cli.format, cli.output.as_deref())
        };
        let opts = cli.detail.options();
        let mut opts = AnalyzeOptions {
            find_duplicates: cli.find_duplicates || cli.full_analysis,
            collections: cli.collections || cli.full_analysis,
            collection_config: cli.collection_config.clone(),
            coll_descs: crate::collection_config::load_collection_descs(
                cli.collection_config.as_deref(),
            ),
            queries: cli.query.clone(),
            query_file: cli.query_file.clone(),
            query_path_depth: cli.query_path_depth,
            // Analyze defaults to raw (all); --reachable-only opts into pruning.
            reachable_only: cli.reachable_only,
            ref_paths: cli.ref_paths,
            field_stats: cli.field_stats,
            obj_graph: cli.obj_graph.is_some() || cli.full_analysis,
            dev_report: cli.dev || cli.bundle_path.is_some(),
            bundle_path: cli.bundle_path.clone(),
            ..opts
        };
        if let Some(n) = cli.hist_root_path_top {
            opts.hist_root_path_top = n;
        }
        opts.report_size = match cli
            .obj_graph
            .as_deref()
            .map(|s| s.to_ascii_lowercase())
            .as_deref()
        {
            Some("medium") => crate::opts::ReportSize::Default,
            Some("large") => crate::opts::ReportSize::Large,
            None | Some("small") => crate::opts::ReportSize::Small,
            Some(other) => {
                eprintln!(
                    "error: unknown --obj-graph tier '{other}' (expected: small, medium, large)"
                );
                std::process::exit(2);
            }
        };
        // --size overrides the collection-detail caps (independent of --obj-graph tier).
        opts.report_size = match cli.size.to_ascii_lowercase().as_str() {
            "small" => crate::opts::ReportSize::Small,
            "default" => crate::opts::ReportSize::Default,
            "large" => crate::opts::ReportSize::Large,
            "max" => crate::opts::ReportSize::Max,
            other => {
                eprintln!(
                    "error: unknown --size tier '{other}' (expected: small, default, large, max)"
                );
                std::process::exit(2);
            }
        };
        // Build the MAT index emitter when --mat DIR is set. The prefix is the
        // input basename with a trailing `.hprof[.gz/.tar.gz]` stripped, matching how
        // MAT names its cache files (`dump_.hprof` -> `dump_.<kind>.index`).
        let mat = match cli.mat.as_deref() {
            Some(dir) => {
                let base = std::path::Path::new(&input)
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("dump");
                let prefix = base
                    .strip_suffix(".hprof.tar.gz")
                    .or_else(|| base.strip_suffix(".hprof.gz"))
                    .or_else(|| base.strip_suffix(".tar.gz"))
                    .or_else(|| base.strip_suffix(".tgz"))
                    .or_else(|| base.strip_suffix(".hprof"))
                    .unwrap_or(base);
                match mat::MatEmitter::new(dir, prefix, cli.mat_binary.as_deref()) {
                    Ok(e) => Some(e),
                    Err(e) => fail(format!(
                        "cannot create MAT index dir '{}': {e}",
                        dir.display()
                    )),
                }
            }
            None => None,
        };
        if let Err(e) = run(
            &input,
            cli.output.as_deref(),
            fmt,
            cli.verbose,
            cvec::Codec::Deflate9,
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

/// Like `analyze_to_report`, but also returns the per-object retained-size array
/// so callers (e.g. the HTTP server) can reuse it for OQL queries without a
/// full re-scan of the dump.
pub(crate) fn analyze_to_report_with_retained(
    source: &crate::source::HprofSource,
    opts: &AnalyzeOptions,
) -> std::io::Result<(crate::report::Report, Vec<u64>)> {
    analyze_to_report_inner(source, opts)
}

fn analyze_to_report_inner(
    source: &crate::source::HprofSource,
    opts: &AnalyzeOptions,
) -> std::io::Result<(crate::report::Report, Vec<u64>)> {
    let p1 = pass1::Pass1::run(source, false)?;

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

    let compress = cvec::Codec::Deflate9;

    let mut no_in_sets = std::collections::HashMap::new();
    let mut no_exists_bools = std::collections::HashMap::new();
    let (
        mut g,
        mut inbound,
        shallow_c,
        class_idx_c,
        alloc_serial_c,
        _query_state,
        _refwalk_csr,
        _string_values,
        _string_values_truncated,
    ) = pass2::Pass2::build(
        source,
        p1,
        compress,
        opts,
        &[],
        &mut no_in_sets,
        &mut no_exists_bools,
    )?;

    inbound.compress_id_map(compress)?;

    let rpo = rpo_dfs::rpo_dfs(g.n, &g.gc_root_indices, &g.fwd_offsets, &g.fwd_targets);

    {
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
    }

    let mut rpo = rpo;
    let parent_pre_count = rpo.parent_pre.len();
    let parent_pre_c = if compress != cvec::Codec::None {
        let c = cvec::CompressedU32::compress(&rpo.parent_pre, compress)?;
        crate::trace::drop_vec(std::mem::take(&mut rpo.parent_pre));
        Some(c)
    } else {
        None
    };

    // Capture type-reference graph (class-pair counts) inline, then per-object
    // edges for the top-500K objects by shallow size (feeds click-through view).
    // g.class_idx is compressed into class_idx_c at this point; restore it
    // transiently for the scan (matching the run() path).
    if opts.obj_graph {
        g.class_idx = class_idx_c.restore()?;
        let (pairs, pair_fields) = crate::pass2::capture_type_ref_graph(&g);
        crate::trace::drop_vec(std::mem::take(&mut g.class_idx));
        g.type_ref_pairs = Some(pairs);
        g.type_ref_pair_fields = Some(pair_fields);
        crate::trace::probe("main: after capture_type_ref_graph");
        g.obj_graph_edges = Some(crate::pass2::capture_obj_graph_edges(
            &g,
            500_000,
            opts.report_size.edge_cap(),
        ));
        crate::trace::probe("main: after capture_obj_graph_edges");
    }

    // When --field-stats is requested, save the fwd CSR before inbound consumes it.
    // It is restored into g after retained computation so build_field_stats can use it.
    let field_stats_fwd: Option<(Vec<u32>, crate::chunkvec::ChunkU32)> = if opts.field_stats {
        let total_edges = g.fwd_offsets.last().copied().unwrap_or(0) as usize;
        let fwd_off_copy = g.fwd_offsets.clone();
        let mut fwd_tgt_copy = crate::chunkvec::ChunkU32::zeroed(total_edges);
        for i in 0..total_edges {
            fwd_tgt_copy.set(i, g.fwd_targets.get(i));
        }
        Some((fwd_off_copy, fwd_tgt_copy))
    } else {
        None
    };

    // build_from_fwd needs dfn alive; it is cleared afterward (matching run()).
    // Large dumps: drop fwd_targets first and rescan HPROF to avoid the
    // inb_flat+fwd_targets peak (saves ~7-10 GB on 30G+ dumps).
    let (inb_block_off, inb_data) = if inbound.total_inb > 1_000_000_000 {
        drop(std::mem::take(&mut g.fwd_targets));
        drop(std::mem::take(&mut g.fwd_offsets));
        inbound.build_mat_scan(&rpo.dfn, |_src, _fwd| Ok(()))?
    } else {
        inbound.build_from_fwd(
            std::mem::take(&mut g.fwd_offsets),
            std::mem::take(&mut g.fwd_targets),
            &rpo.dfn,
        )?
    };

    // Rebuild vertex while dfn is still live; then free dfn.
    let count = parent_pre_count;
    rpo.vertex = rpo_dfs::rebuild_vertex(&rpo.dfn, count);
    rpo.dfn = Vec::new();

    if let Some(c) = parent_pre_c {
        rpo.parent_pre = c.restore()?;
    }

    g.idom =
        dominator::compute_dominators(g.n, rpo, &g.gc_root_indices, &inb_block_off, &inb_data)?;
    drop(inb_block_off);
    drop(inb_data);

    let (dc_off, dc_tgt) = retained::build_dom_children_csr(g.n, &g.idom);

    if compress != cvec::Codec::None {
        g.shallow = shallow_c.restore()?;
        g.class_idx = class_idx_c.restore()?;
    }
    drop(shallow_c);
    drop(class_idx_c);

    let class_count = g.class_names.len();
    let (retained, has_same, depth_counts) = retained::compute_retained(
        g.n,
        &g.shallow,
        &g.class_idx,
        class_count,
        &g.class_obj_class_idx,
        &dc_off,
        &dc_tgt,
    );
    g.retained = retained;
    g.has_same_class_ancestor = has_same;

    // Compute field_stats now (while the saved fwd clone is live and retained is populated),
    // then immediately free the clone to avoid carrying it through build_model's allocations.
    let precomputed_field_stats: Option<crate::report::FieldStats> =
        if let Some((fwd_off, fwd_tgt)) = field_stats_fwd {
            g.fwd_offsets = fwd_off;
            g.fwd_targets = fwd_tgt;
            let fs = crate::report::build_field_stats(&g);
            g.fwd_offsets = Vec::new();
            g.fwd_targets = crate::chunkvec::ChunkU32::default();
            Some(fs)
        } else {
            None
        };

    let alloc_sites = if let Some(c) = alloc_serial_c {
        let mut agg = report::AllocAgg::new(&g, opts.alloc_sites_top);
        c.for_each_u32(|serial| agg.push(serial))?;
        let a = agg.finish();
        g.alloc_frames_by_serial = None;
        Some(a)
    } else {
        let a = report::build_alloc_sites(&g, opts.alloc_sites_top);
        g.alloc_stack_serial = Vec::new();
        g.alloc_frames_by_serial = None;
        Some(a)
    };

    let report = report::build_model(
        &mut g,
        dc_off,
        dc_tgt,
        opts.leak_children_cap,
        &depth_counts,
        opts,
        alloc_sites,
        precomputed_field_stats,
    );
    // dc_off and dc_tgt were moved into build_model and freed early inside it.

    // Extract the per-object retained-size array before g is dropped.
    // The caller (analyze_to_report_with_retained) stores this for OQL reuse.
    let retained = std::mem::take(&mut g.retained);

    Ok((report, retained))
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
    if lower.ends_with(".hprof")
        || lower.ends_with(".hprof.gz")
        || lower.ends_with(".hprof.zip")
        || lower.ends_with(".tar.gz")
        || lower.ends_with(".tgz")
    {
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
    // A genuine dump (HPROF magic present) that hits EOF mid-record is almost
    // always a truncated or partially-copied file. Replace the raw internal
    // reader message with a user-actionable one.
    if e.kind() == io::ErrorKind::UnexpectedEof && looks_like_hprof(input) {
        return format!(
            "'{input}' appears truncated or corrupt — hit end of file mid-record; \
             re-copy the .hprof dump and retry"
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

/// A collected query: its OQL text (with any `-- @viz` directive already
/// stripped), the parsed [`VizSpec`] if one was declared, an optional warning
/// when the directive was malformed (surfaced later as a result note), and an
/// optional display name (config `[[query]]` entries may name themselves).
struct CollectedQuery {
    text: String,
    viz: Option<query::viz::VizSpec>,
    warning: Option<String>,
    name: Option<String>,
}

/// Collect the OQL queries for a run: inline `--query` flags first, then each
/// non-empty, non-comment (`#`) line of `--query-file`, then any `[[query]]`
/// entries from the config file. A leading `-- @viz` line in a query file
/// attaches to the FOLLOWING query line (queries in files are one-per-line, so
/// the directive is its own physical line). Inline `--query` args and config
/// `oql` strings may embed the directive on their own `\n`-separated line. The
/// directive is stripped from the text before parsing; a malformed one is
/// recorded as a warning and the query still runs as a plain table.
/// A missing or unreadable query file is a hard error naming the path.
fn collect_query_texts(opts: &AnalyzeOptions) -> io::Result<Vec<CollectedQuery>> {
    let mut collected: Vec<CollectedQuery> = opts
        .queries
        .iter()
        .map(|q| {
            let (text, viz, warning) = query::viz::split_directive(q);
            CollectedQuery {
                text,
                viz,
                warning,
                name: None,
            }
        })
        .collect();
    if let Some(ref qf) = opts.query_file {
        let body = std::fs::read_to_string(qf).map_err(|e| {
            io::Error::new(e.kind(), format!("cannot read --query-file '{qf}': {e}"))
        })?;
        // A pending `-- @viz` directive line waits for the next query line.
        let mut pending_directive: Option<String> = None;
        let mut line_num = 0usize;
        for line in body.lines() {
            line_num += 1;
            let t = line.trim();
            if t.is_empty() || t.starts_with('#') {
                continue;
            }
            // A `-- @viz ...` line prefixes the next query; hold it.
            if is_viz_directive_line(t) {
                pending_directive = Some(t.to_string());
                continue;
            }
            let full = match pending_directive.take() {
                Some(dir) => format!("{dir}\n{t}"),
                None => t.to_string(),
            };
            let (text, viz, warning) = query::viz::split_directive(&full);
            // Validate parse eagerly so we can attach the line number.
            if let Err(e) = query::parse::parse(&text) {
                let semi_hint = if e.0.contains(';') || text.contains(';') {
                    " (each line is one query; semicolons are not supported — put each query on its own line)"
                } else {
                    ""
                };
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "--query-file '{qf}': parse error on line {line_num}: {}{semi_hint}",
                        e.0
                    ),
                ));
            }
            collected.push(CollectedQuery {
                text,
                viz,
                warning,
                name: None,
            });
        }
    }
    // Config `[[query]]` entries run after CLI-supplied queries, keeping their
    // declared names for display.
    for cq in crate::collection_config::load_config_queries(opts.collection_config.as_deref()) {
        let (text, viz, warning) = query::viz::split_directive(&cq.oql);
        collected.push(CollectedQuery {
            text,
            viz,
            warning,
            name: cq.name,
        });
    }
    Ok(collected)
}

/// True if a trimmed line is a `-- @viz ...` directive (case-insensitive on the
/// `@viz` keyword). Mirrors the recognizer in `query::viz::split_directive` so a
/// directive on its own query-file line is attached to the following query
/// rather than being fed to the OQL parser as a stray comment.
fn is_viz_directive_line(t: &str) -> bool {
    t.strip_prefix("--")
        .map(str::trim_start)
        .and_then(|r| r.split_whitespace().next())
        .is_some_and(|w| w.eq_ignore_ascii_case("@viz"))
}

/// Parse and plan each OQL text, failing fast with an actionable message that
/// names the offending query text and includes the parser/planner detail.
fn parse_plan_queries(
    query_texts: &[String],
    depth_cap: usize,
) -> io::Result<Vec<(query::ast::Query, query::plan::QueryPlan)>> {
    let mut parsed_queries: Vec<(query::ast::Query, query::plan::QueryPlan)> =
        Vec::with_capacity(query_texts.len());
    for text in query_texts {
        let q = query::parse::parse_or_report(text).map_err(|report| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("OQL parse error in `{text}`:\n{report}"),
            )
        })?;
        let plan = query::plan::plan_query(&q, depth_cap).map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("OQL plan error in `{text}`: {}", e.0),
            )
        })?;
        let plan = query::optimize::optimize(plan, &q, &query::optimize::SchemaStats::default());
        parsed_queries.push((q, plan));
    }
    Ok(parsed_queries)
}

/// Fill in each result's display metadata: the OQL text from the source query
/// (the executor leaves `oql` blank) and a default `q{N}` name when unnamed.
/// Relies on `query_results` already being restored to the caller's input
/// order (pass2 sorts it), so the positional zip against `query_texts` and the
/// 1-based `q{N}` labels line up with the queries the user supplied.
///
/// An unnamed block derives a descriptive default from its FROM target (e.g.
/// `java.lang.String`, `INSTANCEOF java.lang.Thread`, `object 0x10`) via
/// [`query::viz::default_view_name`]; subquery/UNION sources have no single
/// class, so those fall back to the positional `q{N}` label. Derived (and
/// already-set) names are de-duplicated so two identical FROM targets render as
/// `java.lang.String` and `java.lang.String (2)`. Explicit config-/directive
/// names still win: they are applied later in [`attach_viz`].
fn finalize_query_labels(
    results: &mut [query::model::QueryResult],
    query_texts: &[String],
    queries: &[(query::ast::Query, query::plan::QueryPlan)],
) {
    use std::collections::HashSet;
    let mut seen: HashSet<String> = HashSet::new();
    for (i, (r, text)) in results.iter_mut().zip(query_texts.iter()).enumerate() {
        if r.oql.is_empty() {
            r.oql = text.clone();
        }
        if r.name.is_empty() {
            let base = queries
                .get(i)
                .map(|(q, _)| q)
                .and_then(query::viz::default_view_name)
                .unwrap_or_else(|| format!("q{}", i + 1));
            let mut name = base.clone();
            let mut n = 2;
            while seen.contains(&name) {
                name = format!("{base} ({n})");
                n += 1;
            }
            r.name = name.clone();
            seen.insert(name);
        } else {
            // An already-set name still participates in de-dup.
            seen.insert(r.name.clone());
        }
    }
}

fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let (m, n) = (a.len(), b.len());
    let mut prev: Vec<usize> = (0..=n).collect();
    let mut curr = vec![0usize; n + 1];
    for i in 1..=m {
        curr[0] = i;
        for j in 1..=n {
            curr[j] = if a[i - 1] == b[j - 1] {
                prev[j - 1]
            } else {
                1 + prev[j - 1].min(prev[j]).min(curr[j - 1])
            };
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[n]
}

/// True when a FROM target is a *plain* class name — a bare dotted identifier,
/// not `INSTANCEOF`, not a double-quoted regex, not a `pkg.*` glob, and not an
/// array (`[]`) target. Only for such a FROM does "zero rows" unambiguously
/// mean "no such class" (a glob/regex/instanceof can legitimately match nothing
/// without any single named class being absent).
#[allow(dead_code)]
fn is_plain_class_from(q: &query::ast::Query) -> Option<&str> {
    let spec = q.from.class_spec()?;
    if spec.instanceof || spec.is_regex {
        return None;
    }
    let name = spec.class_name.as_str();
    if name.contains('*') || name.ends_with("[]") || name.is_empty() {
        return None;
    }
    Some(name)
}

/// Same as `is_plain_class_from` but also matches `FROM INSTANCEOF <class>`,
/// so absent-class annotation works for INSTANCEOF queries too.
fn is_named_class_from(q: &query::ast::Query) -> Option<&str> {
    let spec = q.from.class_spec()?;
    if spec.is_regex {
        return None;
    }
    let name = spec.class_name.as_str();
    if name.contains('*') || name.ends_with("[]") || name.is_empty() {
        return None;
    }
    Some(name)
}

/// Annotate each zero-row result whose query selects `FROM <plain class name>`
/// with a note when that class is absent from the dump, so a typo'd or wrong
/// class name is not silently reported as an empty-but-valid result. The dump's
/// class-name set is resolved lazily (one cheap pass1) and ONLY when at least
/// one result is a zero-row plain-class candidate, so the common (non-empty or
/// non-plain) case pays nothing. Skips UNION-collapsed runs, where result and
/// query indices no longer align one-to-one.
fn annotate_missing_classes(
    input: &str,
    results: &mut [query::model::QueryResult],
    queries: &[(query::ast::Query, query::plan::QueryPlan)],
) {
    // Index alignment (results[i] <-> queries[i]) only holds without UNION
    // collapse, which is exactly when the counts match.
    if results.len() != queries.len() {
        return;
    }
    let has_candidate = results.iter().zip(queries.iter()).any(|(r, (q, _))| {
        r.error.is_none() && r.row_count == 0 && is_named_class_from(q).is_some()
    });
    if !has_candidate {
        return;
    }
    // Resolve the dump's dotted class-name set once (slash-form normalized to
    // dots, matching how FROM names are written and how LiveResolver maps them).
    let Ok(p1) = Pass1::run(&crate::source::HprofSource::from(input), false) else {
        return;
    };
    let names: std::collections::HashSet<String> = p1
        .class_map
        .values()
        .filter_map(|ci| p1.strings.get(&ci.name_id).map(|s| s.replace('/', ".")))
        .collect();
    for (r, (q, _)) in results.iter_mut().zip(queries.iter()) {
        if r.error.is_some() || r.row_count != 0 {
            continue;
        }
        if let Some(name) = is_named_class_from(q) {
            if !names.contains(name) {
                let simple = name.rsplit('.').next().unwrap_or(name);
                let lower = name.to_ascii_lowercase();
                let simple_lower = simple.to_ascii_lowercase();
                // Match by: simple name equality (case-insensitive), or
                // the query name is a substring of a real name, or vice-versa,
                // or the real simple name starts with the query simple name prefix (≥4 chars),
                // or edit distance ≤ 2 on the simple name (catches typos like Stirng→String).
                // Exclude JVM array descriptors (names starting with '[') from suggestions.
                let prefix_len = simple_lower.len().min(6);
                let dist_threshold = if simple_lower.len() <= 4 { 1 } else { 2 };
                let mut candidates: Vec<&str> = names
                    .iter()
                    .filter(|n| {
                        if n.starts_with('[') {
                            return false;
                        }
                        let nl = n.to_ascii_lowercase();
                        let sn = n
                            .rsplit('.')
                            .next()
                            .unwrap_or(n.as_str())
                            .to_ascii_lowercase();
                        sn == simple_lower
                            || nl.contains(&lower)
                            || (prefix_len >= 4 && sn.starts_with(&simple_lower[..prefix_len]))
                            || edit_distance(&sn, &simple_lower) <= dist_threshold
                    })
                    .map(|n| n.as_str())
                    .collect();
                candidates.sort_unstable();
                candidates.dedup();
                candidates.truncate(4);
                let hint = if candidates.is_empty() {
                    format!(
                        "no class named `{name}` in this dump \
                         (check the fully-qualified name, or use a `pkg.*` glob)"
                    )
                } else {
                    format!(
                        "no class named `{name}` in this dump — did you mean: {}?",
                        candidates.join(", ")
                    )
                };
                append_note(r, &hint);
            }
        }
    }
}

/// Attach each collected query's [`VizSpec`] to its result, and fold any
/// directive warning into the result `note`. A well-formed directive whose
/// columns cannot be resolved against the actual result (unknown/non-numeric
/// column, too few columns) is downgraded to a plain table with an explanatory
/// note — charts never hard-fail a query. Results and `collected` are in the
/// same order (executor output is restored to input order before this runs).
fn attach_viz(results: &mut [query::model::QueryResult], collected: &[CollectedQuery]) {
    for (r, c) in results.iter_mut().zip(collected.iter()) {
        // A config-supplied name overrides the positional `q{N}` label.
        if let Some(name) = &c.name {
            if !name.is_empty() {
                r.name = name.clone();
            }
        }
        // A `-- @viz name="..."` directive overrides the label too (inline with
        // the query text, so it wins over the positional/config name).
        if let Some(spec) = &c.viz {
            if let Some(name) = &spec.name {
                if !name.is_empty() {
                    r.name = name.clone();
                }
            }
        }
        // A malformed directive recorded at collection time becomes a note.
        if let Some(w) = &c.warning {
            append_note(r, w);
        }
        // An errored query keeps no chart; the error already explains it.
        if r.error.is_some() {
            continue;
        }
        let Some(spec) = &c.viz else { continue };
        match query::viz::resolve_columns(spec, &r.columns, &r.rows) {
            Ok(_) => r.viz = Some(spec.clone()),
            Err(reason) => append_note(r, &reason),
        }
    }
}

/// Append `msg` to a result's `note`, preserving any existing note.
fn append_note(r: &mut query::model::QueryResult, msg: &str) {
    match &mut r.note {
        Some(existing) => {
            existing.push_str("; ");
            existing.push_str(msg);
        }
        None => r.note = Some(msg.to_string()),
    }
}

/// Format a single query cell for plain-text table output.
fn fmt_query_value(v: &query::model::QueryValue) -> String {
    use query::model::QueryValue::*;
    match v {
        Null => "null".to_string(),
        Bool(b) => b.to_string(),
        Int(i) => i.to_string(),
        Float(f) => f.to_string(),
        Str(s) => s.clone(),
        ObjRef { index, class, addr } => {
            if let Some(a) = addr {
                format!("{class}@0x{a:x}")
            } else {
                format!("{class}@{index}")
            }
        }
    }
}

/// The `query` subcommand: run pass1+pass2 with the parsed queries and print
/// each result as a simple aligned text table (`json_out = false`) or as a
/// JSON array of QueryResult objects (`json_out = true`). Never writes a file.
fn run_queries(input: &str, opts: AnalyzeOptions, json_out: bool) -> io::Result<()> {
    let collected = collect_query_texts(&opts)?;
    if collected.is_empty() {
        // No `--query`, no `--query-file`, and no config `[[query]]` entries: the
        // run would parse the dump and print nothing (exit 0), which reads as a
        // silent success. Fail with an actionable message naming every OQL source.
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "no query given. Supply OQL with `--query \"SELECT ...\"` (repeatable), \
             `--query-file <PATH>` (one per line), a `[[query]]` entry in \
             `.hprof-analyzer.toml`, `--repl` (interactive shell), or `--server` \
             (HTTP endpoint). See `hprof-analyzer query --help`.",
        ));
    }
    let query_texts: Vec<String> = collected.iter().map(|c| c.text.clone()).collect();
    let parsed = parse_plan_queries(&query_texts, opts.query_path_depth)?;
    let (flat, union_groups) = query::run::expand_union_queries(&parsed);

    // Subqueries need a two-phase (inner-then-outer) scan; `run_single_dump`
    // implements that. When any query uses a FROM- or IN-subquery, route through
    // it so the `query` subcommand fully supports subqueries. The inline path
    // below stays for the common no-subquery case (one scan, no re-parse).
    let uses_subqueries = parsed.iter().any(|(_, p)| {
        p.from_subplan.is_some() || !p.in_subplans.is_empty() || !p.exists_subplans.is_empty()
    });

    // Cross-phase queries (retained sizes, dominators, N-hop RefPath, edges,
    // gc-roots) cannot be answered by the query-only fast path — it never builds
    // the dominator tree / retained sizes / edge structures. When any query needs
    // one of those, AUTO-ESCALATE to the full analysis pipeline (`run_oql_escalated`)
    // which builds them and produces real rows, instead of the old "requires the
    // full analysis pipeline" error. RefWalk-only queries also set `ref_walk` and
    // will escalate here; that is harmless (the full pipeline resolves them too).
    let needs_full = parsed.iter().any(|(_, p)| {
        !p.late_ops.is_empty()
            || p.needs.retained
            || p.needs.dominator_children
            || p.needs.ref_walk
            || p.needs.gc_roots
    });

    let mut query_results = if uses_subqueries {
        query::run::run_single_dump(input, &parsed, opts.reachable_only)?
    } else if needs_full {
        run_oql_escalated(input, &flat, &union_groups, opts.reachable_only, &opts)?
    } else {
        let source_q = crate::source::HprofSource::from(input);
        let p1 = pass1::Pass1::run(&source_q, false)?;
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
        let needs_sv = flat.iter().any(|(_, p)| p.needs.string_values);
        let addr_vec = if needs_sv {
            query::run::id_map_to_addrs(&p1.id_map)
        } else {
            Vec::new()
        };
        let mut no_in_sets = std::collections::HashMap::new();
        let mut no_exists_bools = std::collections::HashMap::new();
        let (
            g,
            _inbound,
            _fwd_off_c,
            _fwd_tgt_c,
            _in_c,
            query_state,
            refwalk_csr,
            string_values,
            _sv_trunc,
        ) = pass2::Pass2::build(
            &source_q,
            p1,
            cvec::Codec::Deflate9,
            &opts,
            &flat,
            &mut no_in_sets,
            &mut no_exists_bools,
        )?;

        // Query-only path: retained sizes/dominators are not computed, so cross-phase
        // (@retainedHeapSize) queries resolve to actionable errors here.
        // toString(s) queries (finalize_at=P2) use the decoded string_values map.
        // RefPath (`x.field.tail`) queries use the RefWalk CSR captured in the scan.
        // Reachable-only (the query-subcommand default): the scan armed a per-row
        // source-index sidecar; compute GC-reachability now (one rpo_dfs over the
        // forward CSR pass2 already built) and prune each flat result by its
        // captured source index INSIDE resume, before UNION-collapse, so a
        // projected `@objectAddress` (a raw heap address) prunes by the EXACT
        // source dense index rather than a lossy re-read. Skipped under --all.
        let rpo = opts.reachable_only.then(|| {
            crate::rpo_dfs::rpo_dfs(g.n, &g.gc_root_indices, &g.fwd_offsets, &g.fwd_targets)
        });
        let flat_results = query::run::resume_with_string_values(
            query_state,
            &flat,
            string_values,
            refwalk_csr,
            rpo.as_ref().map(|r| r.dfn.as_slice()),
            &addr_vec,
            None,
        );

        query::run::collapse_union_results(flat_results, &union_groups)
    };

    // Fill in blank oql text and default names (from-target-derived, else
    // `q{N}`) for the printed tables.
    finalize_query_labels(&mut query_results, &query_texts, &parsed);
    annotate_missing_classes(input, &mut query_results, &parsed);
    attach_viz(&mut query_results, &collected);

    if json_out {
        // JSON output: emit a JSON array of QueryResult objects to stdout.
        // Errors are represented inline via the `error` field; always exit 0
        // so the caller can parse the JSON regardless.
        match serde_json::to_string_pretty(&query_results) {
            Ok(j) => println!("{j}"),
            Err(e) => {
                return Err(io::Error::other(format!(
                    "failed to serialize query results as JSON: {e}"
                )));
            }
        }
        return Ok(());
    }

    let mut out = String::new();
    let mut had_error = false;
    for r in query_results.iter() {
        out.push_str(&format!("== {} ==\n", r.name));
        if !r.oql.is_empty() {
            out.push_str(&format!("  {}\n", r.oql));
        }
        if let Some(err) = &r.error {
            had_error = true;
            out.push_str(&format!("error: {err}\n\n"));
            continue;
        }
        let headers: Vec<String> = r.columns.iter().map(|c| c.name.clone()).collect();
        let body: Vec<Vec<String>> = r
            .rows
            .iter()
            .map(|row| row.iter().map(fmt_query_value).collect())
            .collect();
        let ncols = headers.len();
        // Numeric columns (all non-null values are Int or Float) get right-aligned.
        let is_numeric: Vec<bool> = (0..ncols)
            .map(|col| {
                r.rows.iter().all(|row| {
                    matches!(
                        row.get(col),
                        Some(query::model::QueryValue::Int(_))
                            | Some(query::model::QueryValue::Float(_))
                            | Some(query::model::QueryValue::Null)
                            | None
                    )
                }) && r.rows.iter().any(|row| {
                    matches!(
                        row.get(col),
                        Some(query::model::QueryValue::Int(_))
                            | Some(query::model::QueryValue::Float(_))
                    )
                })
            })
            .collect();
        // Compute per-column widths (header width vs max cell width).
        let mut widths: Vec<usize> = headers.iter().map(|h| h.chars().count()).collect();
        for row in &body {
            for (i, cell) in row.iter().enumerate() {
                if i < ncols {
                    widths[i] = widths[i].max(cell.chars().count());
                }
            }
        }
        let pad_left = |s: &str, w: usize| -> String {
            let n = s.chars().count();
            if n >= w {
                s.to_string()
            } else {
                format!("{s}{}", " ".repeat(w - n))
            }
        };
        let pad_right = |s: &str, w: usize| -> String {
            let n = s.chars().count();
            if n >= w {
                s.to_string()
            } else {
                format!("{}{s}", " ".repeat(w - n))
            }
        };
        // Header row — left-aligned (even for numeric columns, conventional).
        let hdr_cells: Vec<String> = headers
            .iter()
            .enumerate()
            .map(|(i, h)| {
                if i + 1 < ncols {
                    pad_left(h, widths[i])
                } else {
                    h.clone()
                }
            })
            .collect();
        out.push_str(&hdr_cells.join(" | "));
        out.push('\n');
        // Separator.
        let sep: Vec<String> = widths.iter().map(|&w| "-".repeat(w)).collect();
        out.push_str(&sep.join("-+-"));
        out.push('\n');
        // Data rows — numeric columns right-aligned, others left-aligned.
        for row in &body {
            let cells: Vec<String> = row
                .iter()
                .enumerate()
                .map(|(i, cell)| {
                    if i + 1 < ncols {
                        if i < ncols && is_numeric[i] {
                            pad_right(cell, widths[i])
                        } else {
                            pad_left(cell, widths[i])
                        }
                    } else {
                        cell.clone()
                    }
                })
                .collect();
            out.push_str(&cells.join(" | "));
            out.push('\n');
        }
        let plural = if r.row_count == 1 { "row" } else { "rows" };
        let trunc = if r.truncated { ", truncated" } else { "" };
        out.push_str(&format!("({} {}{})\n", r.row_count, plural, trunc));
        if let Some(note) = &r.note {
            out.push_str(&format!("note: {note}\n"));
        }
        out.push('\n');
    }
    print!("{out}");
    if had_error {
        return Err(io::Error::other(
            "one or more queries returned an error (see output above)",
        ));
    }
    Ok(())
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
    let source = crate::source::HprofSource::from(input);
    let p1 = pass1::Pass1::run(&source, mat.is_some())?;
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

    // A valid HPROF header with zero objects means the file was truncated before
    // the heap dump segment. Warn on stderr but continue — the report will be
    // empty, which is still more useful than a hard failure.
    if p1.class_ids.is_empty() {
        eprintln!(
            "warning: '{input}' contains no heap objects — \
             the file may be truncated before the heap dump segment"
        );
    }

    // fast (before the expensive graph build) with a message naming the query.
    let collected = collect_query_texts(&opts)?;
    let query_texts: Vec<String> = collected.iter().map(|c| c.text.clone()).collect();
    let parsed_queries = parse_plan_queries(&query_texts, opts.query_path_depth)?;
    // Subqueries require a two-phase (inner-then-outer) scan of the dump. The
    // full-report pipeline scans once and immediately builds the graph +
    // dominators atop that scan, so it cannot run the inner pass here without an
    // invasive restructure. Per the task's accepted fallback, subqueries are
    // supported only via the `query` subcommand (`run_single_dump`), which
    // re-scans; surface an actionable error rather than silently wrong rows.
    if parsed_queries
        .iter()
        .any(|(_, p)| p.from_subplan.is_some() || !p.in_subplans.is_empty())
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "subqueries (FROM (...) / IN (...)) are only supported via the `query` \
             subcommand, which re-scans the dump; they are not available in the \
             full report. Run the query with `hprof-analyzer query <dump> -e '<oql>'`.",
        ));
    }
    // MEMORY-CRITICAL: decide edge retention BEFORE pass2 so a run with no
    // edge-using query (@inbounds / @outbounds / path()) stays byte-for-byte and
    // RSS-identical to today — every new branch below is gated on `run_flags`.
    //
    // `plan_run` needs a `ClassIndexResolver` only to fill `retain_rows` (dense
    // class bits). The dense class universe is built INSIDE pass2 and is not
    // available yet, so here we pass a trivial resolver: this yields the correct
    // BOOLEAN flags (which are computed purely by query inspection). Row-level
    // filtering (L1) is done AFTER pass2 by matching each source row's class name
    // against the edge queries' FROM patterns, so `retain_rows` is not consulted.
    let run_flags = {
        let queries: Vec<query::ast::Query> =
            parsed_queries.iter().map(|(q, _)| q.clone()).collect();
        query::runflags::plan_run(&queries, &NoClassIndex, opts.query_path_depth).map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("OQL edge planning error: {}", e.0),
            )
        })?
    };

    let (flat_queries, union_groups) = query::run::expand_union_queries(&parsed_queries);

    let t = Instant::now();
    progress::phase("building object graph (pass 2)");
    // Capture MAT class metadata from p1 before it is consumed by Pass2::build.
    let mat_class_meta: Option<mat::MatClassMeta> = if mat.is_some() {
        Some(mat::MatClassMeta::from_pass1(&p1))
    } else {
        None
    };
    // Capture hprof file offsets for o2hprof emission. Store as raw LE bytes
    // (CompressedBytes) to avoid a 2x peak during restoration — iterating as
    // u64 chunks avoids the bytes→Vec<u64> copy at emit time.
    let mat_hprof_offsets_c: Option<cvec::CompressedBytes> = if mat.is_some() {
        let bytes: Vec<u8> = {
            let v = &p1.hprof_offsets;
            let mut b = Vec::with_capacity(v.len() * 8);
            for &off in v {
                b.extend_from_slice(&off.to_le_bytes());
            }
            b
        };
        Some(cvec::CompressedBytes::compress(bytes, compress)?)
    } else {
        None
    };
    let mut no_in_sets = std::collections::HashMap::new();
    let mut no_exists_bools = std::collections::HashMap::new();
    let (
        mut g,
        mut inbound,
        shallow_c,
        class_idx_c,
        alloc_serial_c,
        mut query_state,
        refwalk_csr,
        string_values,
        string_values_truncated,
    ) = pass2::Pass2::build(
        &source,
        p1,
        compress,
        &opts,
        &flat_queries,
        &mut no_in_sets,
        &mut no_exists_bools,
    )?;
    log(
        verbose,
        &format!("pass2 n={}", g.n),
        t.elapsed().as_secs_f64(),
    );

    // Take the per-slot source-index sidecar out of the query state now, before it
    // is consumed by `resume` far below. It is populated only when the scan armed
    // reachability capture (`opts.reachable_only`); otherwise it is empty and this
    // is a cheap no-op, keeping the DEFAULT analyze run byte/RSS-identical.
    let row_src_by_slot = query_state.take_row_src_by_slot();

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
    let (mat_coc_snapshot, mat_addrs_c): (
        Option<std::collections::HashMap<u32, u32>>,
        Option<cvec::CompressedU64>,
    ) = if mat.is_some() {
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
        crate::trace::drop_vec(std::mem::take(&mut rpo.parent_pre));
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

    // MEMORY-CRITICAL edge-retention hook. This is the ONLY point where
    // the forward CSR (`g.fwd_offsets`/`g.fwd_targets`) is still alive — it is
    // consumed by `build_from_fwd` just below. When (and ONLY when) an edge query
    // is armed, we build the two bounded, class-filtered edge structures the
    // resume window needs, both from the live forward CSR:
    //
    //   * `retained_edges` (@outbounds / path): a delta+vbyte store keyed by
    //     SOURCE dense index, retaining only rows whose SOURCE object's class
    //     matches an edge query's FROM pattern (L1). Never expands to a flat
    //     Vec<u32> of all edges (L2 — see retained_edges.rs).
    //
    //   * `retained_inbound` (@inbounds): a bounded transpose of the matched-class
    //     forward edges into a plain (in_off,in_tgt) CSR. This is NOT a survival of
    //     the full ~7.5 GB block-encoded inbound CSR (that is freed at its normal
    //     drop below); it is a small index whose target lists are populated only
    //     for nodes whose class matches an @inbounds query's FROM pattern. `in_off`
    //     is full length (n+1) so the executor can index any node safely; only
    //     matched targets carry referrers.
    //
    // Restoring the dense class_idx here costs ~4 GB, so it is guarded INSIDE the
    // `if` — a no-edge run never restores it and the teardown is byte/RSS-identical
    // to today. `CompressedU32::restore(&self)` takes `&self`, so the later restore
    // at the shallow/class_idx window is unaffected.
    let want_forward = run_flags.retain_forward || run_flags.outbounds_by_rescan;
    let want_inbound = run_flags.retain_inbound;
    let (retained_edges, retained_inbound): RetainedEdgeStructs = if want_forward || want_inbound {
        // Collect the FROM (pattern, instanceof) of every query that actually uses
        // an edge feature; class rows are retained only for these (L1).
        let edge_froms: Vec<(String, bool)> = flat_queries
            .iter()
            .filter(|(q, _)| query_uses_edges(q))
            .map(|(q, _)| (q.from.class_name().to_string(), q.from.instanceof()))
            .collect();

        // Dense class index for each node. In the compressed path we restore it
        // locally (~4 GB, freed at block end) since `restore(&self)` is idempotent
        // and the later shallow/class_idx restore is unaffected. In the (rare)
        // no-compress path `g.class_idx` is already dense — borrow it in place so
        // it survives for the later `compute_retained` read.
        let class_idx_restored: Option<Vec<u32>> = if compress != cvec::Codec::None {
            Some(class_idx_c.restore()?)
        } else {
            None
        };
        let class_idx_ref: &[u32] = class_idx_restored
            .as_deref()
            .unwrap_or(g.class_idx.as_slice());
        // A node's class matches when its class name matches any edge query's FROM
        // pattern. `class_name_matches` is glob-aware and separator-normalizing;
        // INSTANCEOF-inclusive matching is not modeled here (bounded superset would
        // require the subclass table), so a plain glob match against the written
        // FROM pattern is used — over-matching only adds retained rows, never drops.
        let node_matches = |s: usize| -> bool {
            let cn = &g.class_names[class_idx_ref[s] as usize];
            edge_froms
                .iter()
                .any(|(pat, _inst)| query::execute::class_name_matches(cn, pat))
        };

        let n = g.n;
        let fwd_off = &g.fwd_offsets;
        let fwd_tgt = &g.fwd_targets;

        // Forward store (@outbounds / path): rows whose SOURCE matches. The
        // forward targets are chunked (`ChunkU32`); `copy_range` gathers one row's
        // out-edges into a scratch Vec (handles chunk-boundary straddles).
        let retained_edges = if want_forward {
            let mut builder = crate::query::retained_edges::RetainedEdgesBuilder::new();
            let mut scratch: Vec<u32> = Vec::new();
            for s in 0..n {
                if !node_matches(s) {
                    continue;
                }
                let (lo, hi) = (fwd_off[s] as usize, fwd_off[s + 1] as usize);
                fwd_tgt.copy_range(lo, hi, &mut scratch);
                scratch.sort_unstable();
                builder.push_row(s as u32, &scratch);
            }
            Some(builder.finish())
        } else {
            None
        };

        // Bounded inbound transpose (@inbounds): for each forward edge s -> t whose
        // TARGET t matches, record s as a referrer of t. Two-pass counting build so
        // in_tgt is sized exactly; in_off is full length (n+1). Each source row's
        // targets are gathered via `copy_range` (chunked store).
        let retained_inbound = if want_inbound {
            let mut in_off = vec![0u32; n + 1];
            let mut row: Vec<u32> = Vec::new();
            // Pass 1: count referrers per matched target.
            for s in 0..n {
                let (lo, hi) = (fwd_off[s] as usize, fwd_off[s + 1] as usize);
                fwd_tgt.copy_range(lo, hi, &mut row);
                for &t in &row {
                    if node_matches(t as usize) {
                        in_off[t as usize + 1] += 1;
                    }
                }
            }
            // Prefix sum -> offsets.
            for i in 0..n {
                in_off[i + 1] += in_off[i];
            }
            let total = in_off[n] as usize;
            let mut in_tgt = vec![0u32; total];
            let mut cursor = in_off.clone();
            // Pass 2: scatter sources into matched targets' slots.
            for s in 0..n {
                let (lo, hi) = (fwd_off[s] as usize, fwd_off[s + 1] as usize);
                fwd_tgt.copy_range(lo, hi, &mut row);
                for &t in &row {
                    if node_matches(t as usize) {
                        let slot = &mut cursor[t as usize];
                        in_tgt[*slot as usize] = s as u32;
                        *slot += 1;
                    }
                }
            }
            Some((in_off, in_tgt))
        } else {
            None
        };

        // Drop the dense class_idx we restored just for filtering (compress path).
        drop(class_idx_restored);
        (retained_edges, retained_inbound)
    } else {
        // No edge query -> zero allocation; forward CSR teardown identical to today.
        (None, None)
    };

    // fwd_offsets and fwd_targets are moved into build_from_fwd so they can be
    // freed INSIDE the call, before Phase 4 allocates inb_data.
    // MAT: snapshot fwd_off (prefix-sum offsets) and class_idx before the
    // inbound scan consumes InboundBuilder. We do NOT snapshot fwd_tgt here —
    // instead we do a post-inbound HPROF rescan to scatter-fill fwd_tgt from
    // scratch using fwd_off as per-object write cursors. This avoids keeping
    // the ~8 GB fwd_targets ChunkU32 alive across the inb_flat allocation and
    // the ~2.5 GB compressed fwd_tgt_c blob from inflating the outbound window.
    let mat_fwd_snap: Option<(cvec::CompressedU32, cvec::CompressedU32)> = if mat.is_some() {
        let class_idx: Vec<u32> = class_idx_c.restore()?;
        let fwd_off_c = cvec::CompressedU32::compress(&g.fwd_offsets, compress)?;
        let class_idx_c2 = cvec::CompressedU32::compress(&class_idx, compress)?;
        drop(class_idx);
        Some((fwd_off_c, class_idx_c2))
    } else {
        None
    };
    // Save the data needed for the later outbound rescan (id_map, class info,
    // field plans) BEFORE InboundBuilder is consumed. class_addr_to_hist and
    // field_plans_dense are moved out (cheap; small data). id_map_c is cloned
    // (~0.5 GB blob). Must be called after compress_id_map.
    let mat_outbound_rescan_ctx: Option<crate::pass2::MatOutboundRescanCtx> = if mat.is_some() {
        Some(inbound.take_for_outbound_rescan())
    } else {
        None
    };

    // Capture the type-reference graph (class-pair edge counts) inline from the
    // live fwd-CSR before it is consumed by inbound construction. This replaces
    // the old approach of iterating all per-object edges in build_type_ref_graph
    // (which required keeping the full 20 GB ObjGraphCapture alive). The inline
    // scan builds only a HashMap of class-pair counts, costing ~200-500 MB peak.
    // NOTE: g.class_idx was compressed inside Pass2 (to cut the binding peak);
    // restore it transiently here for the scan, then drop it again immediately.
    if opts.obj_graph {
        g.class_idx = class_idx_c.restore()?;
        let (pairs, pair_fields) = crate::pass2::capture_type_ref_graph(&g);
        crate::trace::drop_vec(std::mem::take(&mut g.class_idx));
        g.type_ref_pairs = Some(pairs);
        g.type_ref_pair_fields = Some(pair_fields);
        crate::trace::probe("main: after capture_type_ref_graph");
    }

    // Capture per-object edges for top-500K objects by shallow size only.
    // This feeds build_obj_graph_flat (the click-through view), which only
    // displays nodes with high retained size. Objects not captured get
    // edges_unknown=true. The sparse HashMap<u32,Box<[...]>> avoids the 4 GB
    // of CSR offset arrays the old full-universe capture required.
    // 500K × 150 edges × 6 B ≈ 450 MB, vs. the old 24 GB full capture.
    const OBJ_GRAPH_TOP_N: usize = 500_000;
    if opts.obj_graph {
        g.obj_graph_edges = Some(crate::pass2::capture_obj_graph_edges(
            &g,
            OBJ_GRAPH_TOP_N,
            opts.report_size.edge_cap(),
        ));
        crate::trace::probe("main: after capture_obj_graph_edges");
    }

    // When --field-stats is requested, save the fwd CSR before inbound consumes it.
    // It is restored after retained computation so build_field_stats can use it.
    let field_stats_fwd_main: Option<(Vec<u32>, crate::chunkvec::ChunkU32)> = if opts.field_stats {
        let total_edges = g.fwd_offsets.last().copied().unwrap_or(0) as usize;
        let fwd_off_copy = g.fwd_offsets.clone();
        let mut fwd_tgt_copy = crate::chunkvec::ChunkU32::zeroed(total_edges);
        for i in 0..total_edges {
            fwd_tgt_copy.set(i, g.fwd_targets.get(i));
        }
        Some((fwd_off_copy, fwd_tgt_copy))
    } else {
        None
    };

    let (inb_block_off, inb_data) = if mat.is_some() {
        // Drop fwd_targets BEFORE calling build_mat_scan so that inb_flat
        // allocation (6 GB) does not coexist with fwd_targets (8 GB).
        // build_mat_scan rescans the HPROF file to reconstruct inbound without
        // needing fwd_targets. Inbound peak drops from ~25 GB to ~12 GB.
        drop(std::mem::take(&mut g.fwd_targets));
        drop(std::mem::take(&mut g.fwd_offsets)); // no longer needed; InboundBuilder has in_cursors
        inbound.build_mat_scan(
            &rpo.dfn,
            |_src, _fwd| Ok(()), // outbound collected later via HPROF rescan
        )?
    } else if inbound.total_inb > 1_000_000_000 {
        // Large dump: drop fwd_targets before inb_flat alloc so both ~N×4B
        // arrays never coexist. Costs one extra HPROF rescan but cuts the
        // inbound peak by ~7-10 GB on 30G+ production dumps.
        progress::phase("building inbound references (rescan path)");
        drop(std::mem::take(&mut g.fwd_targets));
        drop(std::mem::take(&mut g.fwd_offsets));
        crate::trace::trim();
        crate::trace::probe("main: fwd dropped (rescan-inbound path, before inb_flat alloc)");
        inbound.build_mat_scan(&rpo.dfn, |_src, _fwd| Ok(()))?
    } else {
        crate::trace::probe("main: before build_from_fwd call");
        inbound.build_from_fwd(
            std::mem::take(&mut g.fwd_offsets),
            std::mem::take(&mut g.fwd_targets),
            &rpo.dfn,
        )?
    };
    log(verbose, "inbound", t.elapsed().as_secs_f64());

    crate::trace::trim();

    // Rebuild vertex: dfn is still live and the inbound encode has returned,
    // so the ~2 GB vertex never coexists with inb_flat. vertex = invert(dfn)
    // is a pure O(n) pass; the dominator reads it next.
    let count = parent_pre_count;
    rpo.vertex = rpo_dfs::rebuild_vertex(&rpo.dfn, count);
    crate::trace::probe("main: after rebuild_vertex (post-inbound, dfn live)");
    // Snapshot GC-reachability for `--reachable-only` OQL pruning BEFORE dfn is
    // freed (it is emptied on the next line, then `rpo` is moved into the
    // dominator stage). Guarded on the flag so the DEFAULT analyze run pays no
    // clone and stays byte/RSS-identical. `None` = raw-heap (the analyze default).
    let reach_dfn: Option<Vec<u32>> = if opts.reachable_only {
        Some(rpo.dfn.clone())
    } else {
        None
    };
    // MAT inbound decode needs dfn (dense→pre-order) to build the per-pre-order
    // offset table into inb_data. Save it compressed before clearing; restore
    // just before use so it doesn't inflate the emit_outbound peak (~40 MB win).
    let mat_dfn_save_c: Option<cvec::CompressedU32> = if mat.is_some() {
        let v = std::mem::take(&mut rpo.dfn);
        Some(cvec::CompressedU32::compress(&v, compress)?)
    } else {
        crate::trace::drop_vec(std::mem::take(&mut rpo.dfn));
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
    // Compress g.idom (~2 GB for 514M objects) immediately after dominator so it
    // doesn't inflate the MatIdMap::build + idx/o2hprof emission window. We restore
    // it just before MatIdMap::build, drop it again after, then restore once more
    // before domIn emission. This is a single compress/decompress pair per GB saved.
    let mat_idom_c: Option<cvec::CompressedU32> = if mat.is_some() {
        let c = cvec::CompressedU32::compress(&g.idom, compress)?;
        g.idom = Vec::new();
        crate::trace::trim();
        Some(c)
    } else {
        None
    };
    crate::trace::probe("main: after compress idom (before inb_data compress)");
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
    // minimum possible window. Restore idom for MatIdMap::build, then drop it again.
    let mat_map: Option<mat::MatIdMap> = if let Some(addrs_c) = mat_addrs_c {
        // Restore idom only for MatIdMap::build, then drop immediately after.
        if let Some(ref c) = mat_idom_c {
            g.idom = c.restore()?;
        }
        let addrs = addrs_c.restore()?;
        let mm = mat::MatIdMap::build(g.n, &g.idom, |i| addrs[i]);
        // Drop idom immediately after build; mm.sorted holds old-ids in addr order.
        g.idom = Vec::new();
        crate::trace::trim();
        crate::trace::probe("main: after MatIdMap::build + drop(idom)");
        // emit idx: mat-id 0 = address 0x0 (synthetic root), then in mat-id order.
        // Stream addresses from addrs[sorted[i]] — no extra Vec<i64> needed.
        if let Some(ref m) = mat {
            m.emit_long_index_iter(
                "idx",
                std::iter::once(0i64).chain(
                    mm.sorted()
                        .iter()
                        .map(|&old_id| addrs[old_id as usize] as i64),
                ),
            )?;
            drop(addrs); // free ~4 GB after idx emission
            crate::trace::probe("main: after emit idx + drop(addrs) (before o2hprof)");
        } else {
            drop(addrs);
        }
        Some(mm)
    } else {
        None
    };
    // o2hprof: emit after mat_map block so we can move mat_hprof_offsets_c out
    // (freeing the compressed blob before decompressing, avoiding coexistence of
    // the ~2 GB blob and the ~4 GB decompressed bytes).
    if let (Some(m), Some(mm)) = (mat.as_ref(), mat_map.as_ref()) {
        if let Some(off_c) = mat_hprof_offsets_c {
            // offsets_bytes: flat LE u64 bytes (8 bytes per object). CompressedBytes
            // restore() consumes self — the compressed blob is freed when the method
            // takes ownership, before the output Vec<u8> is fully allocated.
            let offsets_bytes = off_c.restore()?;
            m.emit_long_index_iter(
                "o2hprof",
                std::iter::once(0i64).chain(mm.sorted().iter().map(|&old_id| {
                    let lo = old_id as usize * 8;
                    i64::from_le_bytes(offsets_bytes[lo..lo + 8].try_into().unwrap())
                })),
            )?;
            drop(offsets_bytes);
        }
    } else {
        drop(mat_hprof_offsets_c);
    }
    crate::trace::probe("main: after emit o2hprof");
    crate::trace::probe("main: before mat_inv (free_addrs done at idx emission)");
    // Build the row→class-object id inverse table now that mm is available, so
    // we can prefer reachable class-objects when multiple map to the same row.
    let mut mat_inv: Option<Vec<i32>> =
        if let (Some(mm), Some(coc)) = (mat_map.as_ref(), mat_coc_snapshot.as_ref()) {
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
        let mut name_to_coid: std::collections::HashMap<&str, i32> =
            std::collections::HashMap::new();
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
        let _ = mm; // mm borrow ends here
    };
    // Resolve class_obj_ids from the raw class_idx rows now that mat_inv is ready.
    // mat_fwd_snap.1 holds the compressed class_idx; restore, map through inv,
    // then re-compress so the 45 MB array doesn't inflate the emit_outbound peak.
    let mat_class_obj_ids_c: Option<cvec::CompressedU32> =
        if let (Some(inv), Some(fwd_snap)) = (mat_inv.as_ref(), mat_fwd_snap.as_ref()) {
            let class_idx_rows = fwd_snap.1.restore()?;
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
    crate::trace::trim();
    crate::trace::probe("main: before emit_outbound");
    // g.idom was compressed earlier (after dominator) and dropped during MatIdMap::build.
    // It remains compressed in mat_idom_c; no re-compression needed here.
    // MAT: emit `outbound` IntArray1N in MAT id order.
    // We rebuild the forward CSR by rescanning the HPROF file using
    // mat_outbound_rescan_ctx (id_map + class info). fwd_off is restored from
    // mat_fwd_snap.0 and used as per-object write cursors (modified in-place):
    // after scatter, fwd_off[d] = end position; start[d] = fwd_off[d-1].
    if let Some(ref m) = mat {
        let mm = mat_map.as_ref().expect("mat_map built with mat");
        if let (Some((fwd_off_c, _class_idx_c)), Some(rescan_ctx)) =
            (mat_fwd_snap.as_ref(), mat_outbound_rescan_ctx.as_ref())
        {
            // Restore fwd_off (prefix-sum offsets), drop compressed blob.
            let mut fwd_off = fwd_off_c.restore()?;
            let total_edges = if !fwd_off.is_empty() {
                fwd_off[fwd_off.len() - 1] as usize
            } else {
                0
            };
            // Pre-allocate fwd_tgt and scatter-fill via HPROF rescan.
            let mut fwd_tgt: Vec<u32> = vec![0u32; total_edges];
            crate::trace::probe("main: before outbound rescan (fwd_off+fwd_tgt allocated)");
            crate::pass2::rescan_outbound(rescan_ctx, &mut fwd_off, &mut fwd_tgt)?;
            crate::trace::probe("main: after outbound rescan");
            // id_map was live during rescan (2 GB) and is now freed; trim to return
            // freed pages before restoring class_obj_ids, saving ~2 GB from emit peak.
            crate::trace::trim();
            // Restore class_obj_ids AFTER the rescan to avoid inflating the rescan peak by 2 GB.
            let class_obj_ids = mat_class_obj_ids_c
                .as_ref()
                .expect("mat_class_obj_ids_c built when mat present")
                .restore()?;
            // Emit outbound in MAT id order. After scatter, fwd_off[d] = end pos;
            // start[d] = fwd_off[d-1] (or 0 for d==0).
            // Translate fwd_tgt entries (dense ids) to MAT ids in-place to avoid
            // a large per-object scratch Vec that can inflate peak RSS by 1-2 GB
            // for objects with high out-degree (large arrays, class objects).
            let n_entries = mm.mat_count();
            let sorted = mm.sorted();
            let mut idx = 0usize;
            m.emit_outbound_cb(n_entries, |push| {
                if idx == 0 {
                    idx += 1;
                    return Ok(());
                }
                let old_id = sorted[idx - 1];
                idx += 1;
                let lo = if old_id == 0 {
                    0
                } else {
                    fwd_off[old_id as usize - 1] as usize
                };
                let hi = fwd_off[old_id as usize] as usize;
                let coid = class_obj_ids[old_id as usize];
                let class_mat = if coid == u32::MAX {
                    0i32
                } else {
                    mm.translate(coid as i32).max(0)
                };
                // Translate dense ids to MAT ids in-place; compact reachable ones to front.
                let mut count = 0usize;
                for i in lo..hi {
                    let mid = mm.translate(fwd_tgt[i] as i32);
                    if mid >= 0 {
                        fwd_tgt[lo + count] = mid as u32;
                        count += 1;
                    }
                }
                fwd_tgt[lo..lo + count].sort_unstable();
                // Emit class_mat first, then remaining edges (dedup, skip class_mat).
                let class_u = class_mat as u32;
                push(class_mat)?;
                let mut prev = u32::MAX;
                for &v in &fwd_tgt[lo..lo + count] {
                    if v != class_u && v != prev {
                        push(v as i32)?;
                        prev = v;
                    }
                }
                Ok(())
            })?;
            crate::trace::probe("main: after emit_outbound_cb (before drops)");
            drop(fwd_tgt);
            drop(fwd_off);
            drop(class_obj_ids);
            crate::trace::trim();
        }
    }
    drop(mat_outbound_rescan_ctx);
    drop(mat_fwd_snap);
    drop(mat_class_obj_ids_c);
    crate::trace::probe(
        "main: after drop(mat_fwd_snap) — restore inb_data + build inb offset table",
    );
    // Restore inb_data now (was compressed across emit_outbound to save ~80 MB).
    let inb_data = inb_data_c.restore()?;
    // MAT inbound: build a block-sampled byte-offset table into inb_data.
    // Stores one u64 offset per INB_BLOCK_MAT pre-orders (~256 MB for 513M nodes).
    // Using u64 avoids overflow when inb_data exceeds 4 GB on large dumps.
    // dfn[dense] = pre_order; vertex[pre_order] = dense_id.
    const INB_BLOCK_MAT: usize = 16;
    let mat_inb_ctx: Option<(Vec<u32>, Vec<u64>, Vec<u32>)> =
        if let (Some(dfn_c), Some(vertex_c)) = (mat_dfn_save_c, mat_vertex_save_c) {
            let dfn = dfn_c.restore()?;
            let vertex = vertex_c.restore()?;
            let n = g.n;
            let mut off: Vec<u64> = Vec::with_capacity(n / INB_BLOCK_MAT + 2);
            let mut pos = 0usize;
            for pre in 0..n {
                if pre % INB_BLOCK_MAT == 0 {
                    off.push(pos as u64);
                }
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
    // referrers on demand from inb_data using the block-offset table.
    if let Some(ref m) = mat {
        let mm = mat_map.as_ref().expect("mat_map built with mat");
        if let Some((dfn, inb_pre_off, vertex)) = mat_inb_ctx.as_ref() {
            let iter = std::iter::once(Vec::new()) // entry 0 = synthetic root
                .chain(mm.sorted().iter().map(|&old_id| {
                    let pre = dfn[old_id as usize] as usize;
                    // Seek to block start, then skip (pre % INB_BLOCK_MAT) entries.
                    let block_start = pre - (pre % INB_BLOCK_MAT);
                    let mut pos = inb_pre_off[block_start / INB_BLOCK_MAT] as usize;
                    for _skip in block_start..pre {
                        let (skip_count, c0) = vbyte::decode_one(&inb_data[pos..]);
                        pos += c0;
                        for _ in 0..skip_count {
                            let (_, c1) = vbyte::decode_one(&inb_data[pos..]);
                            pos += c1;
                        }
                    }
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
                            if mid >= 0 {
                                e.push(mid);
                            }
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
    // Restore g.idom from compressed blob (it was compressed before emit_outbound
    // to save ~1.3 GB of RSS during the outbound + inbound emission window).
    if let Some(c) = mat_idom_c {
        g.idom = c.restore()?;
    }
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

    // For the non-MAT path compress g.idom (~2 GB) now that the CSR is built.
    // compute_retained no longer reads idom (uses the stack for parent lookups).
    // Saves ~2 GB during the compute_retained window; decompressed before build_model.
    let non_mat_idom_c: Option<cvec::CompressedU32> =
        if mat.is_none() && compress != cvec::Codec::None {
            let c = cvec::CompressedU32::compress(&g.idom, compress)?;
            g.idom = Vec::new();
            crate::trace::trim();
            Some(c)
        } else {
            None
        };
    crate::trace::probe(
        "main: after compress idom (non-MAT path, before restore shallow/class_idx)",
    );

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
            .filter_map(|&v| {
                let mid = mm.translate(v as i32);
                if mid >= 0 { Some(mid) } else { None }
            })
            .collect();
        let iter = std::iter::once(vroot_children)
            .chain(std::iter::once(Vec::new())) // entry 1: mat-id 0 synthetic root
            .chain(mm.sorted().iter().map(|&old_id| {
                let lo = dc_off[old_id as usize] as usize;
                let hi = dc_off[old_id as usize + 1] as usize;
                dc_tgt[lo..hi]
                    .iter()
                    .filter_map(|&v| {
                        let mid = mm.translate(v as i32);
                        if mid >= 0 { Some(mid) } else { None }
                    })
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

    // Compute field_stats now (while the saved fwd clone is live and retained is populated),
    // then immediately free the clone to avoid carrying it through build_model's allocations.
    let precomputed_field_stats_main: Option<crate::report::FieldStats> =
        if let Some((fwd_off, fwd_tgt)) = field_stats_fwd_main {
            g.fwd_offsets = fwd_off;
            g.fwd_targets = fwd_tgt;
            let fs = crate::report::build_field_stats(&g);
            g.fwd_offsets = Vec::new();
            g.fwd_targets = crate::chunkvec::ChunkU32::default();
            Some(fs)
        } else {
            None
        };

    // Finalize cross-phase (@retainedHeapSize) queries now that retained sizes
    // exist. Phase-1 results pass through; carried indices are joined against
    // g.retained, then all results reassemble in original query order.
    let query_asts: Vec<query::ast::Query> = flat_queries.iter().map(|(q, _)| q.clone()).collect();
    // Dominator stages read idom + the dominator-children CSR (dc_off/dc_tgt),
    // both live in this window. The IdMap is built empty: the dense address
    // table was compressed away at ~L973 (its 4.1GB dense form must not rejoin
    // the RSS peak), and dominator result rows assert on dense indices, not
    // addresses. A later stage that genuinely needs addresses will thread them.
    let id_map = query::stage_runner::IdMap::new(&[]);
    // Thread the query-gated RefWalk CSR into the resume window. Built only when
    // a RefWalk query ran; otherwise the borrowed slices are empty and the shared
    // empty tail map is used, keeping non-RefWalk runs byte/RSS-identical.
    let rw_off: &[u32] = refwalk_csr.as_ref().map_or(&[], |c| &c.fwd_off);
    let rw_tgt: &[u32] = refwalk_csr.as_ref().map_or(&[], |c| &c.fwd_tgt);
    let rw_field: &[u32] = refwalk_csr.as_ref().map_or(&[], |c| &c.fwd_field);
    let rw_names: &[String] = refwalk_csr.as_ref().map_or(&[], |c| &c.field_names);
    let rw_tails = refwalk_csr
        .as_ref()
        .map_or(&*query::stage_runner::EMPTY_REFWALK_TAILS, |c| &c.tails);
    let rw_trunc = refwalk_csr.as_ref().is_some_and(|c| c.truncated);
    // Thread the query-gated edge structures (built at the forward-CSR hook above)
    // into the resume window. Both are `None`/empty on a no-edge run, so the
    // borrowed slices stay empty and behavior is identical to today.
    let in_off: &[u32] = retained_inbound.as_ref().map_or(&[], |(o, _)| o);
    let in_tgt: &[u32] = retained_inbound.as_ref().map_or(&[], |(_, t)| t);
    // Thread the query-gated toString(s) string values into the resume window.
    // Built only when a toString(s) query ran; empty otherwise — non-toString
    // runs keep the shared EMPTY_STRING_VALUES borrow, byte/RSS-identical.
    let sv_ref: &std::collections::HashMap<u32, String> = if string_values.is_empty() {
        &query::stage_runner::EMPTY_STRING_VALUES
    } else {
        &string_values
    };
    // Build the query-gated GC-root-tags lookup (`dense_idx → heap::ROOT_*`) for
    // `@GCRoots`/`@GCRootInfo`/`@info`, ONLY when some plan armed `needs.gc_roots`.
    // Source: zip `g.gc_root_indices` with `g.gc_root_types`, which are aligned
    // 1:1 by construction (both emitted together from the sorted root set in
    // `pass2::mod`; the same pairing `report::build` trusts) — so no fragile
    // address→dense re-derivation is needed and root types can't be mispaired.
    // When no gcroot query ran, the empty static keeps this run byte/RSS-identical.
    let gc_root_tags: std::collections::HashMap<u32, u8> =
        if flat_queries.iter().any(|(_, p)| p.needs.gc_roots) {
            g.gc_root_indices
                .iter()
                .zip(g.gc_root_types.iter())
                .map(|(&idx, &ty)| (idx, ty))
                .collect()
        } else {
            std::collections::HashMap::new()
        };
    let gc_root_tags_ref: &std::collections::HashMap<u32, u8> = if gc_root_tags.is_empty() {
        &query::stage_runner::EMPTY_GC_ROOT_TAGS
    } else {
        &gc_root_tags
    };
    let flat_results = query::stage_runner::resume(
        query_state,
        &query_asts,
        &query::stage_runner::LateCtx {
            retained: &g.retained,
            idom: &g.idom,
            dc_off: &dc_off,
            dc_tgt: &dc_tgt,
            shallow: &g.shallow,
            id_map: &id_map,
            fwd_off: rw_off,
            fwd_tgt: rw_tgt,
            fwd_field: rw_field,
            field_names: rw_names,
            refwalk_tails: rw_tails,
            refwalk_truncated: rw_trunc,
            in_off,
            in_tgt,
            retained_edges: retained_edges.as_ref(),
            string_values: sv_ref,
            string_values_truncated,
            gc_root_tags: gc_root_tags_ref,
            class_idx: &g.class_idx,
            class_names: &g.class_names,
        },
    );

    // `--reachable-only` OQL pruning (opt-in for the analyze command; the default
    // is raw heap, kept byte-identical by the `None` guard). `resume` returns
    // results in slot order (1:1 with `flat_queries`), so `flat_results[i]` is
    // slot `i`. Prune each slot's rows by its scan-captured SOURCE dense index
    // BEFORE UNION-collapse — the same exact-index approach the `query` subcommand
    // uses, so a projected `@objectAddress` (a raw heap address) prunes correctly
    // instead of being mis-read as a dense index. Row-EXPANDING late ops
    // (dominators / AS RETAINED SET / edges) emit rows that are not the original
    // matched objects, so the source sidecar does not align — those slots are left
    // unpruned.
    let mut flat_results = flat_results;
    if let Some(dfn) = &reach_dfn {
        for (slot, r) in flat_results.iter_mut().enumerate() {
            let row_expanding = flat_queries.get(slot).is_some_and(|(_, p)| {
                p.late_ops.iter().any(|op| {
                    matches!(
                        op,
                        query::plan::StageOp::RetainedSet { .. }
                            | query::plan::StageOp::DominatorChildren { .. }
                            | query::plan::StageOp::DominatorOf
                            | query::plan::StageOp::EdgeLookup { .. }
                            | query::plan::StageOp::BoundedPath { .. }
                    )
                })
            });
            if row_expanding {
                continue;
            }
            if let Some(src) = row_src_by_slot.get(&slot) {
                query::run::filter_result_by_src(r, src, dfn);
            }
        }
    }

    let mut query_results = query::run::collapse_union_results(flat_results, &union_groups);

    // Step D: surface the edge-retention note on edge-using result rows. Only
    // present when an edge feature is armed; attaching it to edge-using result
    // rows keeps a no-edge run's JSON byte-identical (`note` is skipped when
    // `None`). After `collapse_union_results`, `query_results` is one row per
    // ORIGINAL top-level query, in `parsed_queries` order — zip against that (a
    // UNION branch's edge use is detected because `plan_run` scans branches too).
    debug_assert_eq!(query_results.len(), parsed_queries.len());
    if let Some(note) = run_flags.retention_note() {
        for (r, (q, _)) in query_results.iter_mut().zip(parsed_queries.iter()) {
            if query_uses_edges(q) && r.note.is_none() {
                r.note = Some(note.clone());
            }
        }
    }

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
            if old_cobj < 0 {
                return None;
            }
            let mat_cid = mm.translate(old_cobj);
            if mat_cid <= 0 {
                return None;
            }
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
    // Restore g.idom (compressed after build_dom_children_csr in the non-MAT path).
    if let Some(c) = non_mat_idom_c {
        g.idom = c.restore()?;
    }
    crate::trace::probe("report: before build_model");
    // build_model reads has_same_class_ancestor (system-overview group) and
    // dc_off/dc_tgt (leak-suspect group) and stores only bounded aggregates,
    // so both can be freed immediately after it returns. depth_counts is the
    // B2 dominator-depth histogram tallied during compute_retained's DFS (no
    // separate ~2GB per-object memo scan).
    let mut report = report::build_model(
        &mut g,
        dc_off,
        dc_tgt,
        opts.leak_children_cap,
        &depth_counts,
        &opts,
        alloc_sites,
        precomputed_field_stats_main,
    );
    crate::trace::probe("report: after build_model");
    g.has_same_class_ancestor = crate::bitset::Bitset::default(); // consumed by build_model
    // dc_off and dc_tgt were moved into build_model and freed early inside it.
    crate::trace::trim();
    // Fill in blank oql text and default names (from-target-derived, else
    // `q{N}`) for the printed tables.
    finalize_query_labels(&mut query_results, &query_texts, &parsed_queries);
    attach_viz(&mut query_results, &collected);
    report.queries = std::mem::take(&mut query_results);
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
            let h = if opts.dev_report {
                html::render_html_dev(&report, opts.bundle_path.as_deref())
            } else {
                html::render_html(&report)
            };
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
    let p = Pass1::run(&crate::source::HprofSource::from(path), false)?;

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

#[cfg(test)]
mod cli_tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn query_path_depth_default_is_5() {
        let cli = Cli::try_parse_from(["hprof-analyzer", "heap.hprof"]).unwrap();
        assert_eq!(cli.query_path_depth, DEFAULT_QUERY_PATH_DEPTH);
        assert_eq!(cli.query_path_depth, 5);
    }

    #[test]
    fn query_path_depth_custom() {
        let cli = Cli::try_parse_from(["hprof-analyzer", "heap.hprof", "--query-path-depth", "3"])
            .unwrap();
        assert_eq!(cli.query_path_depth, 3);
    }

    #[test]
    fn query_path_depth_zero_errors() {
        let err = Cli::try_parse_from(["hprof-analyzer", "heap.hprof", "--query-path-depth", "0"])
            .err()
            .expect("zero depth must be rejected");
        let msg = err.to_string();
        assert!(
            msg.contains("must be > 0"),
            "zero depth must error actionably, got: {msg}"
        );
    }

    #[test]
    fn query_path_depth_non_numeric_errors() {
        let err =
            Cli::try_parse_from(["hprof-analyzer", "heap.hprof", "--query-path-depth", "abc"])
                .err()
                .expect("non-numeric depth must be rejected");
        // clap rejects the non-numeric value at parse time.
        assert!(!err.to_string().is_empty());
    }

    #[test]
    fn query_subcommand_query_path_depth_zero_errors() {
        let err = Cli::try_parse_from([
            "hprof-analyzer",
            "query",
            "heap.hprof",
            "--query-path-depth",
            "0",
        ])
        .err()
        .expect("query subcommand zero depth must be rejected");
        assert!(
            err.to_string().contains("must be > 0"),
            "query subcommand zero depth must error actionably: {err}"
        );
    }

    #[test]
    fn parse_query_path_depth_helper() {
        assert_eq!(parse_query_path_depth("5").unwrap(), 5);
        let zero = parse_query_path_depth("0").unwrap_err();
        assert!(zero.contains("must be > 0"), "0 message: {zero}");
        assert!(
            parse_query_path_depth("abc").is_err(),
            "non-numeric must error"
        );
    }
}

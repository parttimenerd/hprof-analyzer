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
mod md;
#[cfg(test)]
mod md_test;
mod pass1;
mod pass2;
mod progress;
mod query;
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

/// Default depth cap for bounded `path()` walks in edge-query planning; the
/// `--query-path-depth` flag overrides it. Aliases `query::DEFAULT_PATH_DEPTH_CAP`
/// so there is exactly one numeric source of truth.
const DEFAULT_QUERY_PATH_DEPTH: usize = query::DEFAULT_PATH_DEPTH_CAP;

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

/// A `ClassIndexResolver` that resolves nothing — used when only the boolean
/// `RunFlags` (retain_inbound/retain_forward/outbounds_by_rescan) are needed and
/// the dense class universe is unavailable (row filtering is done post-pass2 by
/// class-name match).
struct NoClassIndex;
impl query::runflags::ClassIndexResolver for NoClassIndex {
    fn class_bits(&self, _pattern: &str, _instanceof: bool) -> Vec<usize> {
        Vec::new()
    }
    fn universe_len(&self) -> usize {
        0
    }
}

/// True if this query (or any UNION branch) uses an edge feature
/// (`@inbounds` / `@outbounds` / `path()`).
fn query_uses_edges(q: &query::ast::Query) -> bool {
    query::runflags::plan_run(
        std::slice::from_ref(q),
        &NoClassIndex,
        DEFAULT_QUERY_PATH_DEPTH,
    )
    .map(|f| f.retain_inbound || f.retain_forward || f.outbounds_by_rescan)
    .unwrap_or(false)
}

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
    /// Inline OQL query strings (from `--query`, repeatable).
    pub queries: Vec<String>,
    /// Optional file of OQL queries, one per non-empty/non-comment line.
    pub query_file: Option<String>,
    /// Max hops for OQL `path(a, b)` bounded walks (always > 0).
    pub query_path_depth: usize,
    /// Restrict OQL result rows to GC-reachable objects (Eclipse MAT parity).
    /// The `query` subcommand sets this by default; analyze leaves it off so the
    /// report stays byte-identical unless `--reachable-only` is passed.
    pub reachable_only: bool,
}

impl Default for AnalyzeOptions {
    /// The `--detail default` preset (historical cap values).
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
    #[arg(long, conflicts_with = "all")]
    reachable_only: bool,

    /// Include unreachable objects in OQL results (raw heap scan). The analyze
    /// default; accepted for symmetry with the `query` subcommand.
    #[arg(long)]
    all: bool,
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
    /// Run one or more OQL queries against a heap dump and print the results.
    /// Fast query-only path (no full report): retained-size, dominator, and
    /// reference-graph attributes (@retainedHeapSize, dominators(x), @inbounds,
    /// path(a,b), ...) need the full report instead. See docs/OQL.md.
    Query {
        /// Path to the .hprof (or .hprof.zip) dump.
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
            queries: Vec::new(),
            query_file: None,
            query_path_depth: DEFAULT_QUERY_PATH_DEPTH,
            reachable_only: false,
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
            CompareCmd::Reports { reports, format } => {
                // Name a missing input up front for a clear error, mirroring the
                // MAT arm. Skip "-" (stdin) — it has no filesystem path.
                for p in &reports {
                    if p != "-" && !std::path::Path::new(p).exists() {
                        fail(format!("cannot open '{p}': no such file or directory"));
                    }
                }
                match diff_reports::run(&reports, resolve_format(format, None)) {
                    Ok(text) => print!("{text}"),
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
        }) => {
            if !input_is_hprof(&input) {
                fail(format!(
                    "'{input}' is not an HPROF dump; the `query` subcommand needs a .hprof[.zip] file"
                ));
            }
            if server {
                // Loopback HTTP server: POST OQL, get JSON back. Reads no stdin;
                // --query/--query-file are ignored.
                if let Err(e) =
                    crate::query::server::run_server(&input, query_path_depth, port.unwrap_or(7070))
                {
                    fail(analyze_error_hint(&input, &e));
                }
            } else if repl {
                // Interactive mode reads queries from stdin; --query/--query-file
                // are ignored.
                if let Err(e) = crate::query::repl::run_repl(&input, query_path_depth) {
                    fail(analyze_error_hint(&input, &e));
                }
            } else {
                let opts = AnalyzeOptions {
                    queries: query,
                    query_file,
                    query_path_depth,
                    // Query subcommand defaults to reachable-only (MAT parity);
                    // --all opts back into a raw-heap scan. --reachable-only is
                    // redundant-but-allowed since the default is already true.
                    reachable_only: !all,
                    ..DetailLevel::Default.options()
                };
                // Reuse the analyze pipeline, printing only the query results as text.
                if let Err(e) = run_queries(&input, opts) {
                    fail(analyze_error_hint(&input, &e));
                }
            }
        }
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
            queries: cli.query.clone(),
            query_file: cli.query_file.clone(),
            query_path_depth: cli.query_path_depth,
            // Analyze defaults to raw (all); --reachable-only opts into pruning.
            // --all is the no-op default and stays off here.
            reachable_only: cli.reachable_only,
            ..opts
        };
        if let Err(e) = run(
            &input,
            cli.output.as_deref(),
            fmt,
            cli.verbose,
            cvec::Codec::Zstd3,
            opts,
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
        for line in body.lines() {
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
        ObjRef { index, class } => format!("{class}@{index}"),
    }
}

/// The `query` subcommand: run pass1+pass2 with the parsed queries and print
/// each result as a simple aligned text table to stdout. Never writes a file.
fn run_queries(input: &str, opts: AnalyzeOptions) -> io::Result<()> {
    let collected = collect_query_texts(&opts)?;
    let query_texts: Vec<String> = collected.iter().map(|c| c.text.clone()).collect();
    let parsed = parse_plan_queries(&query_texts, opts.query_path_depth)?;
    let (flat, union_groups) = query::run::expand_union_queries(&parsed);

    // Subqueries need a two-phase (inner-then-outer) scan; `run_single_dump`
    // implements that. When any query uses a FROM- or IN-subquery, route through
    // it so the `query` subcommand fully supports subqueries. The inline path
    // below stays for the common no-subquery case (one scan, no re-parse).
    let uses_subqueries = parsed
        .iter()
        .any(|(_, p)| p.from_subplan.is_some() || !p.in_subplans.is_empty());

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
        let p1 = pass1::Pass1::run(input)?;
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
        let mut no_in_sets = std::collections::HashMap::new();
        let (g, _inbound, _fwd_off_c, _fwd_tgt_c, _in_c, query_state, refwalk_csr, string_values, _sv_trunc) =
            pass2::Pass2::build(input, p1, cvec::Codec::Zstd3, &opts, &flat, &mut no_in_sets)?;

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
        );
        let collapsed = query::run::collapse_union_results(flat_results, &union_groups);
        collapsed
    };

    // Fill in blank oql text and default names (from-target-derived, else
    // `q{N}`) for the printed tables.
    finalize_query_labels(&mut query_results, &query_texts, &parsed);
    attach_viz(&mut query_results, &collected);

    let mut out = String::new();
    for r in query_results.iter() {
        out.push_str(&format!("== {} ==\n", r.name));
        if !r.oql.is_empty() {
            out.push_str(&format!("  {}\n", r.oql));
        }
        if let Some(err) = &r.error {
            out.push_str(&format!("error: {err}\n\n"));
            continue;
        }
        let header: Vec<String> = r.columns.iter().map(|c| c.name.clone()).collect();
        out.push_str(&header.join(" | "));
        out.push('\n');
        for row in &r.rows {
            let cells: Vec<String> = row.iter().map(fmt_query_value).collect();
            out.push_str(&cells.join(" | "));
            out.push('\n');
        }
        let plural = if r.row_count == 1 { "row" } else { "rows" };
        let trunc = if r.truncated { ", truncated" } else { "" };
        out.push_str(&format!("({} {}{})\n\n", r.row_count, plural, trunc));
    }
    print!("{out}");
    Ok(())
}

/// Auto-escalated `query`-subcommand path for cross-phase OQL features
/// (@retainedHeapSize, dominators()/AS RETAINED SET, @inbounds/@outbounds/path,
/// @GCRoots/@GCRootInfo/@info, N-hop RefPath). Mirrors the `run()` analysis
/// pipeline's call sequence (pass1 → pass2 → rpo → inbound → dominators →
/// retained → resume) but SKIPS report generation, alloc-site aggregation,
/// unreachable-retained, and all the RSS-tuning compress/restore dance. It uses
/// `cvec::Codec::None` throughout so the dense arrays stay live — correctness
/// over peak memory (the query subcommand has no RSS contract). Returns the same
/// `Vec<QueryResult>` the fast path produces, so the caller's finalize/print loop
/// is unchanged. `reachable_only` governs final row pruning (skipped under `--all`).
fn run_oql_escalated(
    input: &str,
    flat: &[(query::ast::Query, query::plan::QueryPlan)],
    union_groups: &[query::run::UnionGroup],
    reachable_only: bool,
    opts: &AnalyzeOptions,
) -> io::Result<Vec<query::model::QueryResult>> {
    let p1 = pass1::Pass1::run(input)?;
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

    // Boolean edge-retention flags (purely query-inspection; a trivial resolver
    // suffices — see run()'s note). Escalation cannot fail on planning here since
    // the queries already planned in `parse_plan_queries`; map the error anyway.
    let run_flags = {
        let queries: Vec<query::ast::Query> = flat.iter().map(|(q, _)| q.clone()).collect();
        query::runflags::plan_run(&queries, &NoClassIndex, opts.query_path_depth).map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("OQL edge planning error: {}", e.0),
            )
        })?
    };

    // No compression: dense arrays stay live so no restore dance is needed.
    // NOTE: pass2 leaves g.shallow / g.class_idx DENSE under Codec::None (it only
    // empties them when compress != None), so we read them directly below.
    let compress = cvec::Codec::None;
    let mut no_in_sets = std::collections::HashMap::new();
    let (
        mut g,
        inbound,
        _shallow_c,
        _class_idx_c,
        _alloc_serial_c,
        mut query_state,
        refwalk_csr,
        string_values,
        string_values_truncated,
    ) = pass2::Pass2::build(input, p1, compress, opts, flat, &mut no_in_sets)?;

    // Per-slot source-index sidecar captured during the scan (armed only when
    // `reachable_only`, via `opts.reachable_only` inside pass2). Taken BEFORE the
    // state is consumed by `resume`, so reachable-only pruning keys off the EXACT
    // source dense index rather than re-reading it from a (possibly `@objectAddress`)
    // projected row value. Empty map on `--all`.
    let row_src_by_slot = query_state.take_row_src_by_slot();

    let rpo = rpo_dfs::rpo_dfs(g.n, &g.gc_root_indices, &g.fwd_offsets, &g.fwd_targets);
    // Snapshot dfn for reachability pruning BEFORE rpo is consumed by dominators.
    let reach_dfn: Option<Vec<u32>> = if reachable_only {
        Some(rpo.dfn.clone())
    } else {
        None
    };

    // Edge-retention hook (mirrors run()): build the query-gated forward store
    // and bounded inbound CSR from the LIVE forward CSR. Under Codec::None
    // g.class_idx is dense, so borrow it in place (no restore).
    let want_forward = run_flags.retain_forward || run_flags.outbounds_by_rescan;
    let want_inbound = run_flags.retain_inbound;
    let (retained_edges, retained_inbound): RetainedEdgeStructs = if want_forward || want_inbound {
        let edge_froms: Vec<(String, bool)> = flat
            .iter()
            .filter(|(q, _)| query_uses_edges(q))
            .map(|(q, _)| (q.from.class_name().to_string(), q.from.instanceof()))
            .collect();
        let class_idx_ref: &[u32] = g.class_idx.as_slice();
        let node_matches = |s: usize| -> bool {
            let cn = &g.class_names[class_idx_ref[s] as usize];
            edge_froms
                .iter()
                .any(|(pat, _inst)| query::execute::class_name_matches(cn, pat))
        };

        let n = g.n;
        let fwd_off = &g.fwd_offsets;
        let fwd_tgt = &g.fwd_targets;

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

        let retained_inbound = if want_inbound {
            let mut in_off = vec![0u32; n + 1];
            let mut row: Vec<u32> = Vec::new();
            for s in 0..n {
                let (lo, hi) = (fwd_off[s] as usize, fwd_off[s + 1] as usize);
                fwd_tgt.copy_range(lo, hi, &mut row);
                for &t in &row {
                    if node_matches(t as usize) {
                        in_off[t as usize + 1] += 1;
                    }
                }
            }
            for i in 0..n {
                in_off[i + 1] += in_off[i];
            }
            let total = in_off[n] as usize;
            let mut in_tgt = vec![0u32; total];
            let mut cursor = in_off.clone();
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

        (retained_edges, retained_inbound)
    } else {
        (None, None)
    };

    // Only the dominator/retained late ops actually consume the dominator tree
    // and retained-size array (`JoinRetained`/`DominatorChildren`/`DominatorOf`/
    // `RetainedSet`, surfaced as `needs.retained` / `needs.dominator_children`).
    // RefWalk, edge (`@inbounds`/`@outbounds`/path), gc-root, and string-value
    // ops escalate for their OWN structures and never read dominators. When no
    // planned query needs dominators, SKIP the inbound-transpose +
    // compute_dominators + build_dom_children_csr + compute_retained chain
    // entirely — on a large heap those dominate escalation cost. `g.idom` /
    // `g.retained` / dc_off / dc_tgt then stay empty and the LateCtx borrows
    // empty slices (the ops that would read them do not run).
    let needs_dominators = flat
        .iter()
        .any(|(_, p)| p.needs.retained || p.needs.dominator_children);

    let (dc_off, dc_tgt): (Vec<u32>, Vec<u32>) = if needs_dominators {
        // Transpose the forward CSR into the inbound CSR (consumes fwd CSR).
        let (inb_block_off, inb_data) = inbound.build_from_fwd(
            std::mem::take(&mut g.fwd_offsets),
            std::mem::take(&mut g.fwd_targets),
            &rpo.dfn,
        )?;

        // Rebuild vertex from dfn, then free dfn; parent_pre stays live (never
        // compressed under Codec::None) so compute_dominators reads it directly.
        let mut rpo = rpo;
        let count = rpo.parent_pre.len();
        rpo.vertex = rpo_dfs::rebuild_vertex(&rpo.dfn, count);
        rpo.dfn = Vec::new();

        g.idom = dominator::compute_dominators(
            g.n,
            rpo,
            &g.gc_root_indices,
            &inb_block_off,
            &inb_data,
        )?;
        drop(inb_block_off);
        drop(inb_data);

        let (dc_off, dc_tgt) = retained::build_dom_children_csr(g.n, &g.idom);

        // g.shallow / g.class_idx are dense under Codec::None — no restore needed.
        let class_count = g.class_names.len();
        let (retained, has_same, _depth_counts) = retained::compute_retained(
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
        (dc_off, dc_tgt)
    } else {
        // `rpo` and the forward CSR are simply dropped unused here — no dominator
        // tree, no retained sizes. Empty dc_off/dc_tgt back the (unused) LateCtx
        // dominator-children fields.
        (Vec::new(), Vec::new())
    };

    // Build the LateCtx exactly as run() does and resume the queries.
    let query_asts: Vec<query::ast::Query> = flat.iter().map(|(q, _)| q.clone()).collect();
    let id_map = query::stage_runner::IdMap::new(&[]);
    let rw_off: &[u32] = refwalk_csr.as_ref().map_or(&[], |c| &c.fwd_off);
    let rw_tgt: &[u32] = refwalk_csr.as_ref().map_or(&[], |c| &c.fwd_tgt);
    let rw_field: &[u32] = refwalk_csr.as_ref().map_or(&[], |c| &c.fwd_field);
    let rw_names: &[String] = refwalk_csr.as_ref().map_or(&[], |c| &c.field_names);
    let rw_tails = refwalk_csr
        .as_ref()
        .map_or(&*query::stage_runner::EMPTY_REFWALK_TAILS, |c| &c.tails);
    let rw_trunc = refwalk_csr.as_ref().is_some_and(|c| c.truncated);
    let in_off: &[u32] = retained_inbound.as_ref().map_or(&[], |(o, _)| o);
    let in_tgt: &[u32] = retained_inbound.as_ref().map_or(&[], |(_, t)| t);
    let sv_ref: &std::collections::HashMap<u32, String> = if string_values.is_empty() {
        &query::stage_runner::EMPTY_STRING_VALUES
    } else {
        &string_values
    };
    let gc_root_tags: std::collections::HashMap<u32, u8> =
        if flat.iter().any(|(_, p)| p.needs.gc_roots) {
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
        },
    );

    // Reachable-only pruning (the query-subcommand default; skipped under --all).
    // `stage_runner::resume` returns results in slot order (1:1 with `flat`), so
    // `flat_results[i]` corresponds to slot `i`. Prune each slot's rows by its
    // captured SOURCE dense index BEFORE UNION-collapse, exactly as the fast path
    // does — this handles a projected `@objectAddress` (a raw heap address) which a
    // value-sniffing prune would mis-read as a dense index and wrongly drop.
    //
    // Row-EXPANDING late ops (dominators / AS RETAINED SET / edges) emit rows that
    // are NOT the original matched objects (they are dominators / retained members /
    // referrers), so the source sidecar no longer aligns 1:1 with the output rows
    // and "was the SOURCE object reachable?" is not the right question for them.
    // Those slots are left unpruned (their captured src, if any, is skipped).
    let mut flat_results = flat_results;
    if let Some(dfn) = &reach_dfn {
        for (slot, r) in flat_results.iter_mut().enumerate() {
            let row_expanding = flat.get(slot).is_some_and(|(_, p)| {
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

    let results = query::run::collapse_union_results(flat_results, union_groups);

    Ok(results)
}

/// The two query-gated edge structures built at the forward-CSR hook: the
/// forward store (`@outbounds`/`path`) and a bounded inbound `(in_off, in_tgt)`
/// CSR (`@inbounds`). Both `None` on a no-edge run.
type RetainedEdgeStructs = (
    Option<crate::query::retained_edges::RetainedEdges>,
    Option<(Vec<u32>, Vec<u32>)>,
);

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

    // Collect + parse + plan any OQL queries before pass2, so a bad query fails
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
    let mut no_in_sets = std::collections::HashMap::new();
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
    ) = pass2::Pass2::build(input, p1, compress, &opts, &flat_queries, &mut no_in_sets)?;
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
    // Snapshot GC-reachability for `--reachable-only` OQL pruning BEFORE dfn is
    // freed (it is emptied on the next line, then `rpo` is moved into the
    // dominator stage). Guarded on the flag so the DEFAULT analyze run pays no
    // clone and stays byte/RSS-identical. `None` = raw-heap (the analyze default).
    let reach_dfn: Option<Vec<u32>> = if opts.reachable_only {
        Some(rpo.dfn.clone())
    } else {
        None
    };
    rpo.dfn = Vec::new();
    crate::trace::trim();

    // Restore parent_pre from compressed blob before the dominator stage.
    if let Some(c) = parent_pre_c {
        rpo.parent_pre = c.restore()?;
        crate::trace::probe("main: after restore parent_pre (before dominator)");
    }

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
    let mut report = report::build_model(
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

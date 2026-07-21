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

    /// Run an OQL query against the heap and include results in the report.
    /// May be repeated. Example: --query "SELECT * FROM java.lang.String"
    #[arg(long = "query", value_name = "OQL")]
    query: Vec<String>,

    /// Read one OQL query per non-empty line from a file (lines starting with `#` are comments).
    #[arg(long = "query-file", value_name = "PATH", value_hint = ValueHint::FilePath)]
    query_file: Option<String>,
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
    Query {
        /// Path to the .hprof (or .hprof.zip) dump.
        #[arg(value_hint = ValueHint::FilePath)]
        input: String,
        /// OQL query text (may be repeated).
        #[arg(long = "query", value_name = "OQL")]
        query: Vec<String>,
        /// Read queries from a file, one per line (`#` comments allowed).
        #[arg(long = "query-file", value_name = "PATH", value_hint = ValueHint::FilePath)]
        query_file: Option<String>,
        /// Start an interactive OQL REPL reading queries from stdin.
        #[arg(long)]
        repl: bool,
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
            repl,
        }) => {
            if !input_is_hprof(&input) {
                fail(format!(
                    "'{input}' is not an HPROF dump; the `query` subcommand needs a .hprof[.zip] file"
                ));
            }
            if repl {
                // Interactive mode reads queries from stdin; --query/--query-file
                // are ignored.
                if let Err(e) = crate::query::repl::run_repl(&input) {
                    fail(analyze_error_hint(&input, &e));
                }
            } else {
                let opts = AnalyzeOptions {
                    queries: query,
                    query_file,
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

/// Collect the OQL query strings for a run: inline `--query` flags first, then
/// each non-empty, non-comment (`#`) line of `--query-file`. A missing or
/// unreadable query file is a hard error naming the path.
fn collect_query_texts(opts: &AnalyzeOptions) -> io::Result<Vec<String>> {
    let mut query_texts: Vec<String> = opts.queries.clone();
    if let Some(ref qf) = opts.query_file {
        let body = std::fs::read_to_string(qf)
            .map_err(|e| io::Error::new(e.kind(), format!("cannot read --query-file '{qf}': {e}")))?;
        for line in body.lines() {
            let t = line.trim();
            if t.is_empty() || t.starts_with('#') {
                continue;
            }
            query_texts.push(t.to_string());
        }
    }
    Ok(query_texts)
}

/// Parse and plan each OQL text, failing fast with an actionable message that
/// names the offending query text and includes the parser/planner detail.
fn parse_plan_queries(
    query_texts: &[String],
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
        let plan = query::plan::plan_query(&q).map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("OQL plan error in `{text}`: {}", e.0),
            )
        })?;
        parsed_queries.push((q, plan));
    }
    Ok(parsed_queries)
}

/// Fill in each result's display metadata: the OQL text from the source query
/// (the executor leaves `oql` blank) and a default `q{N}` name when unnamed.
/// Relies on `query_results` already being restored to the caller's input
/// order (pass2 sorts it), so the positional zip against `query_texts` and the
/// 1-based `q{N}` labels line up with the queries the user supplied.
fn finalize_query_labels(results: &mut [query::model::QueryResult], query_texts: &[String]) {
    for (i, (r, text)) in results.iter_mut().zip(query_texts.iter()).enumerate() {
        if r.oql.is_empty() {
            r.oql = text.clone();
        }
        if r.name.is_empty() {
            r.name = format!("q{}", i + 1);
        }
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
    let query_texts = collect_query_texts(&opts)?;
    let parsed = parse_plan_queries(&query_texts)?;
    let (flat, union_groups) = query::run::expand_union_queries(&parsed);

    // Subqueries need a two-phase (inner-then-outer) scan; `run_single_dump`
    // implements that. When any query uses a FROM- or IN-subquery, route through
    // it so the `query` subcommand fully supports subqueries. The inline path
    // below stays for the common no-subquery case (one scan, no re-parse).
    let uses_subqueries = parsed
        .iter()
        .any(|(_, p)| p.from_subplan.is_some() || !p.in_subplans.is_empty());
    let mut query_results = if uses_subqueries {
        query::run::run_single_dump(input, &parsed)?
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
        let (.., query_state) =
            pass2::Pass2::build(input, p1, cvec::Codec::Zstd3, &opts, &flat, &mut no_in_sets)?;

        // Query-only path: retained sizes/dominators are not computed, so cross-phase
        // (@retainedHeapSize) queries resolve to actionable errors here.
        let flat_results = query::stage_runner::resume_without_late_ctx(query_state);
        query::run::collapse_union_results(flat_results, &union_groups)
    };

    // Fill in blank oql text and default `q{N}` names for the printed tables.
    finalize_query_labels(&mut query_results, &query_texts);

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
    let query_texts = collect_query_texts(&opts)?;
    let parsed_queries = parse_plan_queries(&query_texts)?;
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
    let (flat_queries, union_groups) = query::run::expand_union_queries(&parsed_queries);

    let t = Instant::now();
    progress::phase("building object graph (pass 2)");
    let mut no_in_sets = std::collections::HashMap::new();
    let (mut g, mut inbound, shallow_c, class_idx_c, alloc_serial_c, query_state) =
        pass2::Pass2::build(input, p1, compress, &opts, &flat_queries, &mut no_in_sets)?;
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
    let query_asts: Vec<query::ast::Query> =
        flat_queries.iter().map(|(q, _)| q.clone()).collect();
    // Dominator stages read idom + the dominator-children CSR (dc_off/dc_tgt),
    // both live in this window. The IdMap is built empty: the dense address
    // table was compressed away at ~L973 (its 4.1GB dense form must not rejoin
    // the RSS peak), and dominator result rows assert on dense indices, not
    // addresses. A later stage that genuinely needs addresses will thread them.
    let id_map = query::stage_runner::IdMap::new(&[]);
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
            // RefWalk (Task 27) forward-ref CSR is not live in this window — the
            // production CSR is freed by the inbound transpose before dominators,
            // and no per-edge field-id column is built. Empty slices make the
            // resolver a no-op until the pipeline threads the CSR here.
            fwd_off: &[],
            fwd_tgt: &[],
            fwd_field: &[],
            field_names: &[],
        },
    );
    let mut query_results = query::run::collapse_union_results(flat_results, &union_groups);

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
    // Fill in blank oql text and default `q{N}` names for the printed tables.
    finalize_query_labels(&mut query_results, &query_texts);
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

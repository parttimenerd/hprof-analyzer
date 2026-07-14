//! CLI entry point and two-pass orchestration for the HPROF heap-dump analyzer.
//!
//! Subcommands: `analyze` (parse a dump and emit a report), `diff` (compare a
//! MAT report against our JSON), `diff-reports` (cross-dump growth), `render`
//! (re-render a saved Report JSON), and `dev` (diagnostics).
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
mod reader;
mod report;
mod retained;
mod rpo_dfs;
mod sweep;
mod trace;
mod types;
mod vbyte;

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

/// Opt-in heavy-analysis toggles and their per-analysis caps. All disabled by
/// default; the cap defaults (30/50/20/5000/20) come from clap `default_value_t`
/// on the CLI args, not from `Default` (which zeros the caps).
#[derive(Clone, Copy, Default)]
pub struct AnalyzeOptions {
    pub root_paths: bool,
    pub root_path_max_depth: usize, // default 30
    pub alloc_sites: bool,
    pub alloc_sites_top: usize, // default 50
    pub thread_locals: bool,
    pub thread_locals_per_thread: usize, // default 20
    pub dominator_tree: bool,
    pub dominator_tree_max_nodes: usize, // default 5000
    pub dominator_tree_max_depth: usize, // default 20
}

/// Referrer-graph context preserved (compressed) only under `--root-paths`, so
/// the leak-suspect builder can walk literal inbound edges to GC roots. Holds
/// the inbound CSR (`inb_block_off`/`inb_data`) and the pre-order->node `vertex`
/// map deflated; `restore_*` rebuild the live arrays just-in-time for the
/// bounded per-suspect walk, which drops them immediately after.
pub struct RootPathCtx {
    inb_block_off: Vec<u8>, // deflate of the u64 LE bytes
    inb_block_off_len: usize,
    inb_data: Vec<u8>, // deflate of the raw bytes
    vertex: crate::cvec::CompressedU32,
}

impl RootPathCtx {
    /// Compress the three referrer-walk arrays for low-RSS hold across the rest
    /// of the pipeline. Only ever called under `--root-paths`.
    fn compress(inb_block_off: &[u64], inb_data: &[u8], vertex: &[u32]) -> std::io::Result<Self> {
        let mut block_off_bytes = Vec::with_capacity(inb_block_off.len() * 8);
        for &x in inb_block_off {
            block_off_bytes.extend_from_slice(&x.to_le_bytes());
        }
        let block_off_blob = crate::cvec::deflate_bytes(&block_off_bytes)?;
        let data_blob = crate::cvec::deflate_bytes(inb_data)?;
        let vertex_c = crate::cvec::CompressedU32::compress(vertex, cvec::Codec::Deflate9)?;
        Ok(Self {
            inb_block_off: block_off_blob,
            inb_block_off_len: inb_block_off.len(),
            inb_data: data_blob,
            vertex: vertex_c,
        })
    }

    /// Rebuild the inbound CSR block-offset array (byte-identical to input).
    pub fn restore_inb_block_off(&self) -> std::io::Result<Vec<u64>> {
        let bytes = crate::cvec::inflate_bytes(&self.inb_block_off, self.inb_block_off_len * 8)?;
        Ok(bytes
            .chunks_exact(8)
            .map(|c| u64::from_le_bytes([c[0], c[1], c[2], c[3], c[4], c[5], c[6], c[7]]))
            .collect())
    }

    /// Rebuild the inbound CSR data blob (byte-identical to input).
    pub fn restore_inb_data(&self) -> std::io::Result<Vec<u8>> {
        // Deflate stores no length; inflate reads to EOF (cap only pre-sizes).
        crate::cvec::inflate_bytes(&self.inb_data, self.inb_data.len())
    }

    /// Rebuild the pre-order->node `vertex` map (byte-identical to input).
    pub fn restore_vertex(&self) -> std::io::Result<Vec<u32>> {
        self.vertex.restore()
    }
}

use clap::{Parser, Subcommand, ValueEnum};

/// Top-level CLI: a single subcommand.
#[derive(Parser)]
#[command(
    name = "hprof-analyzer",
    version,
    about = "Analyze Java HPROF heap dumps (Eclipse MAT parity)"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

/// The analyzer subcommands.
#[derive(Subcommand)]
enum Cmd {
    /// Analyze a heap dump and write a report (Markdown or JSON)
    Analyze {
        /// Path to the input .hprof heap dump.
        input: String,
        /// Output path; writes to stdout when omitted.
        output: Option<String>,
        /// Report output format.
        #[arg(short, long, value_enum, default_value_t = FormatArg::Md)]
        format: FormatArg,
        /// Override the per-suspect dominated-children list cap.
        #[arg(long)]
        leak_children_cap: Option<usize>,
        /// Log per-phase timing (and RSS on Linux) to stderr.
        #[arg(short, long)]
        verbose: bool,
        /// Emit RSS probe/trim traces at pipeline checkpoints.
        #[arg(long)]
        trace_rss: bool,
        /// Add a literal reference chain from each leak suspect to a GC root
        /// (opt-in; may raise peak memory well above the default ceiling).
        #[arg(long)]
        root_paths: bool,
        /// Max hops to walk when building a root path.
        #[arg(long, default_value_t = 30)]
        root_path_max_depth: usize,
        /// Aggregate objects by allocation stack-trace serial (opt-in). Reports
        /// nothing useful unless the JVM ran with allocation tracking enabled.
        #[arg(long)]
        alloc_sites: bool,
        /// Keep only the top-N allocation sites by object count.
        #[arg(long, default_value_t = 50)]
        alloc_sites_top: usize,
        /// List a bounded sample of each thread's local root objects (opt-in).
        #[arg(long)]
        thread_locals: bool,
        /// Max local objects to list per thread.
        #[arg(long, default_value_t = 20)]
        thread_locals_per_thread: usize,
        /// Emit the full multi-level dominator subtree per accumulation point
        /// (opt-in; output can be large and may raise peak memory).
        #[arg(long)]
        dominator_tree: bool,
        /// Cap total nodes in a dominator subtree (heaviest kept first).
        #[arg(long, default_value_t = 5000)]
        dominator_tree_max_nodes: usize,
        /// Cap dominator-subtree depth.
        #[arg(long, default_value_t = 20)]
        dominator_tree_max_depth: usize,
    },
    /// Compare a MAT report against our canonical JSON (exit 2 on FAIL)
    Diff {
        /// Path to the Eclipse MAT report (HTML/text).
        mat: String,
        /// Path to our canonical Report JSON.
        ours: String,
        /// Diff output format.
        #[arg(short, long, value_enum, default_value_t = FormatArg::Md)]
        format: FormatArg,
    },
    /// Cross-dump growth diff: compare two canonical Report JSONs (A=baseline, B=current)
    DiffReports {
        /// Baseline (earlier) Report JSON, or "-" for stdin
        a: String,
        /// Current (later) Report JSON
        b: String,
        #[arg(short, long, value_enum, default_value_t = FormatArg::Md)]
        format: FormatArg,
    },
    /// Render a saved canonical Report JSON to Markdown or JSON
    Render {
        /// Path to a Report JSON, or "-" for stdin
        input: String,
        #[arg(short, long, value_enum, default_value_t = FormatArg::Md)]
        format: FormatArg,
    },
    /// Developer / diagnostic commands
    Dev {
        #[command(subcommand)]
        cmd: DevCmd,
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

/// Parse args and dispatch to the selected subcommand.
fn main() {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Analyze {
            input,
            output,
            format,
            leak_children_cap,
            verbose,
            trace_rss,
            root_paths,
            root_path_max_depth,
            alloc_sites,
            alloc_sites_top,
            thread_locals,
            thread_locals_per_thread,
            dominator_tree,
            dominator_tree_max_nodes,
            dominator_tree_max_depth,
        } => {
            if trace_rss {
                trace::set_enabled(true);
            }
            let cap = leak_children_cap.unwrap_or(report::DOMINATED_CAP);
            let opts = AnalyzeOptions {
                root_paths,
                root_path_max_depth,
                alloc_sites,
                alloc_sites_top,
                thread_locals,
                thread_locals_per_thread,
                dominator_tree,
                dominator_tree_max_nodes,
                dominator_tree_max_depth,
            };
            if let Err(e) = run(
                &input,
                output.as_deref(),
                format.into(),
                verbose,
                cvec::Codec::Deflate9,
                cap,
                opts,
            ) {
                eprintln!("Error: {e}");
                process::exit(1);
            }
        }
        Cmd::Diff { mat, ours, format } => {
            let json_out = OutputFormat::from(format) == OutputFormat::Json;
            match diff::run_diff(&mat, &ours, json_out) {
                Ok(true) => {}
                Ok(false) => process::exit(2),
                Err(e) => {
                    eprintln!("Error: {e}");
                    process::exit(1);
                }
            }
        }
        Cmd::DiffReports { a, b, format } => match diff_reports::run(&a, &b, format.into()) {
            Ok(text) => print!("{text}"),
            Err(e) => {
                eprintln!("Error: {e}");
                process::exit(1);
            }
        },
        Cmd::Render { input, format } => match render_report(&input, format.into()) {
            Ok(text) => print!("{text}"),
            Err(e) => {
                eprintln!("Error: {e}");
                process::exit(1);
            }
        },
        Cmd::Dev { cmd } => match cmd {
            DevCmd::EmitSchema => {
                let schema = schemars::schema_for!(report::Report);
                match serde_json::to_string_pretty(&schema) {
                    Ok(js) => println!("{js}"),
                    Err(e) => {
                        eprintln!("Error: {e}");
                        process::exit(1);
                    }
                }
            }
            DevCmd::SweepAggregate { dir } => match sweep::run_aggregate(&dir) {
                Ok(true) => {}
                Ok(false) => process::exit(2),
                Err(e) => {
                    eprintln!("Error: {e}");
                    process::exit(1);
                }
            },
            DevCmd::DumpPass1 { input } => {
                if let Err(e) = dump_pass1_json(&input) {
                    eprintln!("Error: {e}");
                    process::exit(1);
                }
            }
        },
    }
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
fn render_report(path: &str, format: OutputFormat) -> io::Result<String> {
    use std::io::Read;
    let json = if path == "-" {
        let mut buf = String::new();
        io::stdin().read_to_string(&mut buf)?;
        buf
    } else {
        std::fs::read_to_string(path)?
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
    leak_children_cap: usize,
    opts: AnalyzeOptions,
) -> io::Result<()> {
    let t_total = Instant::now();

    let t = Instant::now();
    let p1 = pass1::Pass1::run(input, opts.alloc_sites)?;
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
    let (mut g, mut inbound, shallow_c, class_idx_c) =
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
    let (inb_block_off, inb_data) = inbound.build(&rpo.dfn)?;
    log(verbose, "inbound", t.elapsed().as_secs_f64());
    // Rebuild vertex now: dfn is still live and inbound.build (the binding-peak
    // 2b scan) has returned, so the 1.96GB vertex never coexists with inb_flat.
    // vertex = invert(dfn) is a pure O(n) pass; the dominator reads it next.
    let count = rpo.parent_pre.len();
    rpo.vertex = rpo_dfs::rebuild_vertex(&rpo.dfn, count);
    // Under --root-paths, clone the ~2GB vertex before rpo is moved into
    // compute_dominators; default path never clones (zero RSS change).
    let saved_vertex: Option<Vec<u32>> = if opts.root_paths {
        Some(rpo.vertex.clone())
    } else {
        None
    };
    crate::trace::probe("main: after rebuild_vertex (post-inbound, dfn live)");
    rpo.dfn = Vec::new();
    crate::trace::trim();

    let t = Instant::now();
    // rpo moved by value; vertex/parent_pre owned through translation. dfn
    // already freed above. No separate drop(rpo).
    g.idom =
        dominator::compute_dominators(g.n, rpo, &g.gc_root_indices, &inb_block_off, &inb_data)?;
    log(verbose, "dominator", t.elapsed().as_secs_f64());
    let root_ctx: Option<RootPathCtx> = if opts.root_paths {
        // Under --root-paths we must keep the inbound CSR + vertex alive for the
        // later leak-suspect walk, but holding them dense would blow the budget.
        // Compress into small blobs HERE (right after dominators, the last dense
        // consumer) and free the dense arrays immediately, so nothing carries a
        // second live copy of the ~7.5GB CSR+vertex into the retained phase.
        let ctx = RootPathCtx::compress(
            &inb_block_off,
            &inb_data,
            saved_vertex
                .as_deref()
                .expect("vertex saved under root_paths"),
        )?;
        drop(saved_vertex);
        drop(inb_block_off);
        drop(inb_data);
        Some(ctx)
    } else {
        // Default path: byte-identical to today — free the CSR immediately.
        // Nothing downstream reads it, so there is no compressed copy to keep.
        drop(inb_block_off);
        drop(inb_data);
        None
    };
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

    let t = Instant::now();
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
        leak_children_cap,
        &depth_counts,
        &opts,
        root_ctx,
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

    match output {
        Some(path) => {
            std::fs::write(path, &out_text).map_err(|e| io::Error::new(e.kind(), e))?;
        }
        None => print!("{}", out_text),
    }

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

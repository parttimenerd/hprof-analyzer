mod bitset;
mod chunkvec;
mod cvec;
mod diff;
mod dominator;
mod id_map;
mod md;
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
    Md,
    Json,
}

use clap::{Parser, Subcommand, ValueEnum};

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

#[derive(Subcommand)]
enum Cmd {
    /// Analyze a heap dump and write a report (Markdown or JSON)
    Analyze {
        input: String,
        output: Option<String>,
        #[arg(short, long, value_enum, default_value_t = FormatArg::Md)]
        format: FormatArg,
        #[arg(short, long, value_enum, default_value_t = CompressArg::Deflate9)]
        compress: CompressArg,
        #[arg(long)]
        leak_children_cap: Option<usize>,
        #[arg(short, long)]
        verbose: bool,
        #[arg(long)]
        trace_rss: bool,
    },
    /// Compare a MAT report against our canonical JSON (exit 2 on FAIL)
    Diff {
        mat: String,
        ours: String,
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

#[derive(Subcommand)]
enum DevCmd {
    /// Print the JSON Schema of the report model
    EmitSchema,
    /// Aggregate per-dump *.diff.json files into a gate report (exit 2 on gate-fail)
    SweepAggregate { dir: String },
    /// Dump pass-1 parse stats as JSON
    DumpPass1 { input: String },
}

#[derive(Clone, Copy, PartialEq, ValueEnum)]
enum FormatArg {
    Md,
    Json,
}

#[derive(Clone, Copy, PartialEq, ValueEnum)]
enum CompressArg {
    None,
    Deflate9,
}

impl From<FormatArg> for OutputFormat {
    fn from(f: FormatArg) -> Self {
        match f {
            FormatArg::Md => OutputFormat::Md,
            FormatArg::Json => OutputFormat::Json,
        }
    }
}
impl From<CompressArg> for cvec::Codec {
    fn from(c: CompressArg) -> Self {
        match c {
            CompressArg::None => cvec::Codec::None,
            CompressArg::Deflate9 => cvec::Codec::Deflate9,
        }
    }
}

fn main() {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Analyze {
            input,
            output,
            format,
            compress,
            leak_children_cap,
            verbose,
            trace_rss,
        } => {
            if trace_rss {
                trace::set_enabled(true);
            }
            let cap = leak_children_cap.unwrap_or(report::DOMINATED_CAP);
            if let Err(e) = run(
                &input,
                output.as_deref(),
                format.into(),
                verbose,
                compress.into(),
                cap,
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
        OutputFormat::Json => serde_json::to_string_pretty(&report).map_err(io::Error::other)?,
    })
}

fn run(
    input: &str,
    output: Option<&str>,
    format: OutputFormat,
    verbose: bool,
    compress: cvec::Codec,
    leak_children_cap: usize,
) -> io::Result<()> {
    let t_total = Instant::now();

    let t = Instant::now();
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
    let (mut g, mut inbound, shallow_c, class_idx_c) = pass2::Pass2::build(input, p1, compress)?;
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
    crate::trace::probe("main: after rebuild_vertex (post-inbound, dfn live)");
    rpo.dfn = Vec::new();
    crate::trace::trim();

    let t = Instant::now();
    // rpo moved by value; vertex/parent_pre owned through translation. dfn
    // already freed above. No separate drop(rpo).
    g.idom =
        dominator::compute_dominators(g.n, rpo, &g.gc_root_indices, &inb_block_off, &inb_data)?;
    log(verbose, "dominator", t.elapsed().as_secs_f64());
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
    let class_count = g.class_names.len();
    let (retained, has_same) = retained::compute_retained(
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
    // so both can be freed immediately after it returns.
    let report = report::build_model(&g, &dc_off, &dc_tgt, leak_children_cap);
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
        OutputFormat::Json => {
            // serde_json over a struct preserves field declaration order and
            // carries no f64 (pct is #[serde(skip)]), so output is
            // deterministic. The model holds only KB-scale aggregates, so
            // serialization is trivially RSS-safe even for huge dumps.
            let js = serde_json::to_string_pretty(&report).map_err(io::Error::other)?;
            crate::trace::probe("report: after serialize_json");
            js
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

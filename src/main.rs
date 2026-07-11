mod dominator;
mod id_map;
mod pass1;
mod bitset;
mod pass2;
mod reader;
mod report;
mod retained;
mod rpo_dfs;
mod cvec;
mod trace;
mod types;
mod vbyte;

use std::{env, io, process, time::Instant};

use pass1::Pass1;

fn main() {
    let args: Vec<String> = env::args().collect();

    let mut dump_json = false;
    let mut verbose = false;
    let mut compress = cvec::Codec::Deflate9;
    let mut positional: Vec<&str> = Vec::new();
    for arg in args.iter().skip(1) {
        match arg.as_str() {
            "--dump-json" => dump_json = true,
            "--verbose" | "-v" => verbose = true,
            "--trace-rss" => trace::set_enabled(true),
            s if s.starts_with("--compress=") => {
                let val = &s["--compress=".len()..];
                match cvec::Codec::parse(val) {
                    Some(c) => compress = c,
                    None => {
                        eprintln!("unknown --compress codec '{val}' (use: none, deflate9)");
                        process::exit(1);
                    }
                }
            }
            _ => positional.push(arg.as_str()),
        }
    }

    if positional.is_empty() {
        eprintln!("usage: hprof-analyzer [--verbose] [--dump-json] [--trace-rss] [--compress=none|deflate9] <file.hprof[.gz]> [output.md]");
        process::exit(1);
    }

    let input = positional[0];
    let output = positional.get(1).copied();

    if dump_json {
        match dump_pass1_json(input) {
            Ok(()) => {}
            Err(e) => { eprintln!("Error: {e}"); process::exit(1); }
        }
        return;
    }

    match run(input, output, verbose, compress) {
        Ok(()) => {}
        Err(e) => { eprintln!("Error: {e}"); process::exit(1); }
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
                    let kb: u64 = rest.split_whitespace().next()
                        .and_then(|v| v.parse().ok()).unwrap_or(0);
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

fn run(input: &str, output: Option<&str>, verbose: bool, compress: cvec::Codec) -> io::Result<()> {
    let t_total = Instant::now();

    let t = Instant::now();
    let p1 = pass1::Pass1::run(input)?;
    log(verbose, "pass1", t.elapsed().as_secs_f64());

    let t = Instant::now();
    let (mut g, mut inbound) = pass2::Pass2::build(input, p1)?;
    log(verbose, &format!("pass2 n={}", g.n), t.elapsed().as_secs_f64());

    // Compress the three cold arrays (shallow, class_idx, id_map) that sit idle
    // across the rpo -> inbound -> dominator peak window, freeing their dense
    // Vecs and holding only small blobs. Restored just before each consumer.
    let t = Instant::now();
    let shallow_c = cvec::CompressedU32::compress(&g.shallow, compress)?;
    let class_idx_c = cvec::CompressedU32::compress(&g.class_idx, compress)?;
    if compress != cvec::Codec::None {
        g.shallow = Vec::new();
        g.class_idx = Vec::new();
    }
    inbound.compress_id_map(compress)?;
    log(verbose, "compress-cold", t.elapsed().as_secs_f64());

    let t = Instant::now();
    let mut rpo = rpo_dfs::rpo_dfs(g.n, &g.gc_root_indices, &g.fwd_offsets, &g.fwd_targets);
    log(verbose, "rpo", t.elapsed().as_secs_f64());

    // Free forward CSR (no longer needed after DFS)
    g.fwd_offsets = Vec::new();
    g.fwd_targets = Vec::new();

    // Build the inbound CSR now that rpo has freed its arrays — keeps the
    // ~5.5GB inbound CSR off the rpo-phase RSS peak.
    let t = Instant::now();
    let (inb_offsets, inb_data) = inbound.build()?;
    log(verbose, "inbound", t.elapsed().as_secs_f64());

    let t = Instant::now();
    g.idom = dominator::compute_dominators(
        g.n,
        &rpo,
        &g.gc_root_indices,
        &inb_offsets,
        &inb_data,
    );
    log(verbose, "dominator", t.elapsed().as_secs_f64());
    drop(inb_offsets);
    drop(inb_data);

    // rpo's dfn/vertex/parent_pre are dead after dominator; only rpo_order is
    // still needed (by retained's size loop). Move it out and free the rest
    // (~5GB @514M) before the retained peak window.
    let rpo_order = std::mem::take(&mut rpo.rpo_order);
    drop(rpo);

    // Restore shallow/class_idx (dominator has freed the inbound CSR, so this
    // decompress spike lands outside the peak window).
    if compress != cvec::Codec::None {
        g.shallow = shallow_c.restore()?;
        g.class_idx = class_idx_c.restore()?;
    }
    drop(shallow_c);
    drop(class_idx_c);

    // Build the dominator-children CSR ONCE and share it across compute_retained
    // (hasSame DFS) and report::leak_suspects (both previously rebuilt it, ~6GB
    // redundant @514M).
    let (dc_off, dc_tgt) = retained::build_dom_children_csr(g.n, &g.idom);

    let t = Instant::now();
    let class_count = g.class_names.len();
    let (retained, has_same) = retained::compute_retained(
        g.n,
        rpo_order,
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
    let mut md = String::new();
    md.push_str(&report::system_overview(&g));
    g.has_same_class_ancestor = crate::bitset::Bitset::default(); // only system_overview reads it
    md.push_str(&report::leak_suspects(&g, &dc_off, &dc_tgt));
    drop(dc_off);
    drop(dc_tgt);
    md.push_str(&report::top_consumers(&g));
    log(verbose, "report", t.elapsed().as_secs_f64());

    match output {
        Some(path) => {
            std::fs::write(path, &md).map_err(|e| io::Error::new(e.kind(), e))?;
        }
        None => print!("{}", md),
    }

    log(verbose, "total", t_total.elapsed().as_secs_f64());
    Ok(())
}

fn dump_pass1_json(path: &str) -> io::Result<()> {
    let p = Pass1::run(path)?;

    let mut class_hist: std::collections::HashMap<String, u64> =
        std::collections::HashMap::new();
    for &cid in &p.class_ids {
        if let Some(ci) = p.class_map.get(&cid) {
            let name = p
                .strings
                .get(&ci.name_id)
                .cloned()
                .unwrap_or_else(|| format!("unknown@{cid:#x}"));
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
        if !first { print!(","); }
        let escaped = name.replace('"', "\"");
        print!(r#""{escaped}":{count}"#);
        first = false;
    }
    print!("}}");

    println!("}}");
    Ok(())
}

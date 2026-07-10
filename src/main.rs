mod dominator;
mod id_map;
mod pass1;
mod pass2;
mod reader;
mod report;
mod retained;
mod rpo_dfs;
mod types;
mod vbyte;

use std::{env, io, process, time::Instant};

use pass1::Pass1;

fn main() {
    let args: Vec<String> = env::args().collect();

    let mut dump_json = false;
    let mut verbose = false;
    let mut positional: Vec<&str> = Vec::new();
    for arg in args.iter().skip(1) {
        match arg.as_str() {
            "--dump-json" => dump_json = true,
            "--verbose" | "-v" => verbose = true,
            _ => positional.push(arg.as_str()),
        }
    }

    if positional.is_empty() {
        eprintln!("usage: hprof-analyzer [--verbose] [--dump-json] <file.hprof[.gz]> [output.md]");
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

    match run(input, output, verbose) {
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

fn run(input: &str, output: Option<&str>, verbose: bool) -> io::Result<()> {
    let t_total = Instant::now();

    let t = Instant::now();
    let p1 = pass1::Pass1::run(input)?;
    log(verbose, "pass1", t.elapsed().as_secs_f64());

    let t = Instant::now();
    let mut g = pass2::Pass2::build(input, p1)?;
    log(verbose, &format!("pass2 n={}", g.n), t.elapsed().as_secs_f64());

    let t = Instant::now();
    let rpo = rpo_dfs::rpo_dfs(g.n, &g.gc_root_indices, &g.fwd_offsets, &g.fwd_targets);
    log(verbose, "rpo", t.elapsed().as_secs_f64());

    // Free forward CSR (no longer needed after DFS)
    g.fwd_offsets = Vec::new();
    g.fwd_targets = Vec::new();

    let t = Instant::now();
    g.idom = dominator::compute_dominators(
        g.n,
        &rpo,
        &g.gc_root_indices,
        &g.inb_offsets,
        &g.inb_data,
    );
    log(verbose, "dominator", t.elapsed().as_secs_f64());

    let t = Instant::now();
    let class_count = g.class_names.len();
    let (retained, has_same) = retained::compute_retained(
        g.n,
        &rpo.rpo_order,
        &g.idom,
        &g.shallow,
        &g.class_idx,
        class_count,
        &g.class_obj_class_idx,
    );
    g.retained = retained;
    g.has_same_class_ancestor = has_same;
    log(verbose, "retained", t.elapsed().as_secs_f64());

    let t = Instant::now();
    let mut md = String::new();
    md.push_str(&report::system_overview(&g));
    md.push_str(&report::leak_suspects(&g));
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

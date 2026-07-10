mod id_map;
mod pass1;
mod reader;
mod types;
mod vbyte;
mod pass2;

use std::{env, process};

use pass1::Pass1;

fn main() {
    let args: Vec<String> = env::args().collect();

    // parse flags
    let mut dump_json = false;
    let mut positional: Vec<&str> = Vec::new();
    for arg in args.iter().skip(1) {
        if arg == "--dump-json" {
            dump_json = true;
        } else {
            positional.push(arg.as_str());
        }
    }

    if positional.is_empty() {
        eprintln!("usage: hprof-analyzer [--dump-json] <file.hprof[.gz]> [output.md]");
        process::exit(1);
    }

    let input = positional[0];
    let _output = positional.get(1).copied();

    if dump_json {
        match dump_pass1_json(input) {
            Ok(()) => {}
            Err(e) => { eprintln!("Error: {e}"); process::exit(1); }
        }
        return;
    }

    eprintln!("Input: {input:?}  (full analysis not yet implemented)");
}

fn dump_pass1_json(path: &str) -> std::io::Result<()> {
    let p = Pass1::run(path)?;

    // Build class histogram: class name -> instance count
    // For instances, map class_id -> name via class_map + strings
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

    // Deduplicate GC root addresses and count unique roots
    let mut unique_roots: std::collections::HashSet<u64> =
        std::collections::HashSet::new();
    for &a in &p.gc_root_addrs {
        unique_roots.insert(a);
    }

    // Emit JSON manually (no serde dependency)
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
        // escape any quotes in class name
        let escaped = name.replace('"', "\\\"");
        print!(r#""{escaped}":{count}"#);
        first = false;
    }
    print!("}}");

    println!("}}");
    Ok(())
}

// analysis_full.rhai — Beyond-OQL heap dump investigation (closures + floats)
// =============================================================================
// Closures and floats are fully enabled. Prelude provides:
//   map, filter, sort_by, head, sum, count, print_table, query, pp,
//   walk_tree, investigate, json
//
// Usage:
//   hprof-analyzer heap script rhai dump.hprof --file analysis_full.rhai
//   hprof-analyzer heap script rhai dump.hprof --file analysis_full.rhai --with-graph
// =============================================================================

let SEP  = "════════════════════════════════════════════════════════════════════════";
let SEP2 = "────────────────────────────────────────────────";

fn section(n, title) { print(`\n${SEP}\n  §${n}  ${title}\n${SEP}`); }
fn sub(title)        { print(`\n  ${SEP2}\n  ${title}\n  ${SEP2}`); }

// =============================================================================
section(1, "TRIAGE — heap overview");
// =============================================================================

let info       = dump_info();
let heap_bytes = info["heap_bytes"];
let n_objects  = info["objects"];

print(`  Dump    : ${info["dump"]}`);
print(`  Objects : ${fmt_int(n_objects)}  (object_count=${fmt_int(object_count())})`);
print(`  Classes : ${fmt_int(info["classes"])}`);
print(`  Heap    : ${format_bytes(heap_bytes)}`);
print(`  Graph   : ${if info["graph"] { "loaded" } else { "not loaded (use --with-graph)" }}`);

// =============================================================================
section(2, "REPORT HIGHLIGHTS — triage, leaks, waste");
// OQL has no concept of pre-computed reports or leak heuristics.
// =============================================================================

let secs = report_sections();
print(`  Available report sections: ${secs.len()}`);

let triage = report("triage");
if triage != () && triage.contains("verdict") {
    print(`  Verdict : ${triage["verdict"]}`);
}

let waste = report("waste");
if waste != () && waste.contains("waste_summary") {
    let ws = waste["waste_summary"];
    if ws != () && ws.contains("total_wasted_bytes") {
        print(`  Estimated waste : ${format_bytes(ws["total_wasted_bytes"])}`);
    }
}

let leaks = report("leaks");
if leaks != () {
    print(`  Leak report loaded (use pp(report(\"leaks\")) to explore)`);
}

// =============================================================================
section(3, "CLASS HISTOGRAM with heap% (OQL cannot compute % of total heap)");
// =============================================================================

let hist = histogram_limit(20);
print(`  ${"CLASS".pad_with("",50)}  ${"COUNT".pad_with("",8)}  ${"RETAINED".pad_with("",10)}  HEAP%`);

let rank = 0;
for h in hist {
    rank += 1;
    let cls   = h["class"];
    let shown = if cls.len() > 46 { cls.sub_string(0, 46) + ".." } else { cls };
    let heappct = pct(h["retained_bytes"], heap_bytes);
    print(`  ${rank}. ${shown}  ${fmt_int(h["instances"])}  ${format_bytes(h["retained_bytes"])}  ${heappct}`);
}

// =============================================================================
section(4, "STATISTICAL OUTLIER DETECTION (closures over class_stats)");
// OQL can ORDER BY retainedHeapSize but cannot compute per-class avg/max
// and flag objects that deviate significantly from their class baseline.
// =============================================================================

let stats = class_stats(100);

// CLOSURE: filter to classes where max >> average
let outliers = filter(stats, |s| {
    s["instances"] > 5
    && s["avg_retained"] > 0
    && s["max_retained"] > s["avg_retained"] * 10
});

if outliers.len() == 0 {
    print("  No strong outliers found.");
} else {
    print(`  Found ${outliers.len()} classes with max_retained > 10x avg:`);
    for s in head(outliers, 10) {
        let ratio = s["max_retained"] / (s["avg_retained"] + 1);
        print(`  ${s["class"]}  avg=${format_bytes(s["avg_retained"])}  max=${format_bytes(s["max_retained"])}  ratio=${ratio}x`);

        // OQL CANNOT DO THIS: inspect the outlier object itself
        let obj   = inspect(s["max_retained_idx"]);
        let depth = dominator_depth(s["max_retained_idx"]);
        let idom_idx = obj["idom"];
        let parent_cls = if idom_idx >= 0 { class_name(idom_idx) } else { "<root>" };
        print(`    -> outlier [${s["max_retained_idx"]}] depth=${depth}  idom: ${parent_cls}`);
    }
}

// =============================================================================
section(5, "ACCUMULATION POINT CASCADE (multi-level tree traversal)");
// OQL selects objects by field values. It cannot walk the dominator tree,
// compute multi-level drops, or classify what a subtree contains.
// =============================================================================

let threshold = heap_bytes / 100;  // 1% of heap
if threshold < 1048576 { threshold = 1048576; }

// NOTE: min_ratio is f64 — use 10.0 not 10
let suspects = accumulation_points(threshold, 10.0, 10);

if suspects.len() == 0 {
    print("  No accumulation points above threshold.");
} else {
    let rank = 0;
    for s in suspects {
        rank += 1;
        let heappct = pct(s["retained"], heap_bytes);
        // ratio is now a proper float
        let ratio_f  = s["ratio"];
        print(`\n  [${rank}/${suspects.len()}] [idx=${s["idx"]}] ${s["class"]}`);
        print(`    shallow=${format_bytes(s["shallow"])}  retained=${format_bytes(s["retained"])}  ratio=${ratio_f}x  heap%=${heappct}`);

        // OQL CANNOT DO THIS: walk the dominator path upward
        let path = dominator_path(s["idx"]);
        if path.len() > 1 {
            print("    Retained-by chain:");
            let depth = 0;
            for p in path {
                depth += 1;
                let indent = "";
                let d = depth;
                while d > 0 { indent += "  "; d -= 1; }
                print(`    ${indent}-> [${p["idx"]}] ${p["class"]} (${format_bytes(p["retained"])})`);
            }
        }

        // OQL CANNOT DO THIS: children with retained-size drops
        let drops = big_drops(s["idx"], 4);
        if drops.len() > 0 {
            print("    Biggest children:");
            for d in drops {
                print(`    +- [${d["idx"]}] ${d["class"]}  ${format_bytes(d["retained"])}  (drop: ${format_bytes(d["drop"])})`);
            }
        }

        // OQL CANNOT DO THIS: class breakdown of the retained set
        let rs = retained_set_summary(s["idx"]);
        if rs.len() > 0 {
            print("    Retained set top 4 classes:");
            let shown = 0;
            for r in rs {
                if shown >= 4 { break; }
                shown += 1;
                print(`    |  ${r["class"]}  ${fmt_int(r["count"])} instances  ${format_bytes(r["retained"])}`);
            }
        }

        // OQL CANNOT DO THIS: drill one level deeper into biggest child
        if drops.len() > 0 {
            let child_idx   = drops[0]["idx"];
            let child_drops = big_drops(child_idx, 3);
            if child_drops.len() > 0 {
                print(`    Drilling into [${child_idx}] ${drops[0]["class"]}:`);
                for cd in child_drops {
                    print(`      +- [${cd["idx"]}] ${cd["class"]}  ${format_bytes(cd["retained"])}`);
                }
            }
        }
    }
}

// =============================================================================
section(6, "CROSS-CLASS RETENTION COMPARISON (using closures + map/filter)");
// OQL can query instances of one class at a time.  It cannot compare
// which classes retain byte[], String, and HashMap simultaneously and
// then identify universal containers across all target types.
// =============================================================================

let targets = ["byte[]", "char[]", "java.lang.String", "java.util.HashMap"];
let retainer_votes = #{};

for tgt in targets {
    let tot = class_total(tgt);
    let heappct = pct(tot["retained"], heap_bytes);
    print(`\n  ${tgt}: ${fmt_int(tot["instances"])} instances  ${format_bytes(tot["retained"])}  (${heappct} of heap)`);

    let retainers = who_retains(tgt, 5);
    for r in retainers {
        print(`    <- ${r["class"]}  holds ${fmt_int(r["instances"])}  (${format_bytes(r["total_retained"])})`);
        let cls = r["class"];
        if retainer_votes.contains(cls) {
            retainer_votes[cls] += 1;
        } else {
            retainer_votes[cls] = 1;
        }
    }
}

// CLOSURE + filter: find universal containers
let vote_arr = [];
for cls in retainer_votes.keys() {
    vote_arr.push(#{ class: cls, votes: retainer_votes[cls] });
}
let universal = filter(vote_arr, |v| v["votes"] >= 3);
universal = sort_by(universal, "votes");

if universal.len() > 0 {
    print("\n  Classes retaining 3+ target types (universal containers):");
    for u in universal {
        print(`    ${u["class"]}  (retains ${u["votes"]} of ${targets.len()} target types)`);
    }
}

// =============================================================================
section(7, "DOMINATOR TREE STRUCTURE — fan-out and depth distribution");
// OQL has no concept of the dominator tree structure.
// =============================================================================

sub("Depth distribution for top 500 objects");

// CLOSURE + map: enrich top objects with dominator depth
let tops = top_objects(500);
let depth_arr = map(tops, |obj| dominator_depth(obj["idx"]));

let depth_hist = #{};
for d in depth_arr {
    let capped = if d > 15 { 15 } else { d };
    let key = `${capped}`;
    if depth_hist.contains(key) { depth_hist[key] += 1; } else { depth_hist[key] = 1; }
}

// Build sortable array
let depth_rows = [];
for k in depth_hist.keys() {
    depth_rows.push(#{ depth: parse_int(k), count: depth_hist[k] });
}
depth_rows = sort_by(depth_rows, "depth", false);

for row in depth_rows {
    let bar_len = row["count"] / 5;
    let bar = "";
    let bl = bar_len;
    while bl > 0 { bar += "#"; bl -= 1; }
    print(`  depth ${row["depth"]}  ${fmt_int(row["count"])} objects  ${bar}`);
}

// =============================================================================
section(8, "PACKAGE-LEVEL MEMORY ROLLUP (custom grouping, impossible in OQL)");
// OQL cannot GROUP BY derived fields. Scripts extract the package prefix
// and aggregate retained bytes across all classes in that package.
// =============================================================================

let full_hist = histogram_limit(500);

let pkg_retained = #{};
let pkg_objects  = #{};

for h in full_hist {
    let cls = h["class"];
    // Find first dot to get top-level package
    let top_pkg = cls;
    let dot_pos = cls.index_of(".");
    if dot_pos >= 0 { top_pkg = cls.sub_string(0, dot_pos); }

    if pkg_retained.contains(top_pkg) {
        pkg_retained[top_pkg] += h["retained_bytes"];
        pkg_objects[top_pkg]  += h["instances"];
    } else {
        pkg_retained[top_pkg] = h["retained_bytes"];
        pkg_objects[top_pkg]  = h["instances"];
    }
}

let pkg_rows = [];
for pkg in pkg_retained.keys() {
    pkg_rows.push(#{
        package:  pkg,
        retained: pkg_retained[pkg],
        objects:  pkg_objects[pkg],
    });
}
pkg_rows = sort_by(pkg_rows, "retained");

print(`  ${"PACKAGE".pad_with("",20)}  ${"OBJECTS".pad_with("",8)}  ${"RETAINED".pad_with("",10)}  HEAP%`);
let shown = 0;
for row in pkg_rows {
    if shown >= 15 { break; }
    shown += 1;
    let pkg = row["package"];
    if pkg.len() > 20 { pkg = pkg.sub_string(0, 20); }
    print(`  ${pkg}  ${fmt_int(row["objects"])}  ${format_bytes(row["retained"])}  ${pct(row["retained"], heap_bytes)}`);
}

// =============================================================================
section(9, "CHAIN DETECTION + CLOSURE ENRICHMENT");
// =============================================================================

let chains = detect_chains(3, 20);

if chains.len() == 0 {
    print("  No chains of length >= 3 found.");
} else {
    // CLOSURE + map: enrich each chain with shallow/retained ratio of head
    let enriched = map(chains, |c| #{
        class:          c["class"],
        length:         c["length"],
        total_retained: c["total_retained"],
        head_sh:        shallow(c["idx"]),
        head_ret:       retained(c["idx"]),
    });
    // CLOSURE + filter: only chains with ratio > 5
    let deep_chains = filter(enriched, |c| c["head_sh"] > 0 && c["head_ret"] > c["head_sh"] * 5);
    deep_chains = sort_by(deep_chains, "total_retained");

    print(`  ${chains.len()} chains found, ${deep_chains.len()} with ratio > 5x:`);
    for c in head(deep_chains, 8) {
        let ratio = if c["head_sh"] > 0 { c["head_ret"] / c["head_sh"] } else { 0 };
        print(`  len=${c["length"]}  ${c["class"]}  total=${format_bytes(c["total_retained"])}  ratio=${ratio}x`);
    }
}

// =============================================================================
section(10, "OQL ENRICHMENT PIPELINE (OQL + dominator context)");
// OQL finds objects by field values. Scripts then cross-reference with
// the dominator tree — something OQL has no access to.
// =============================================================================

let maps = oql("SELECT @objectId AS idx, @usedHeapSize AS shallow, @retainedHeapSize AS retained FROM java.util.HashMap ORDER BY retained DESC LIMIT 8");

if maps.len() > 0 {
    // CLOSURE + map: enrich each OQL row with dominator info
    let enriched_maps = map(maps, |row| {
        let idx   = row["idx"];
        let par   = idom(idx);
        let pcls  = if par >= 0 { class_name(par) } else { "<root>" };
        let depth = dominator_depth(idx);
        #{
            idx:       idx,
            shallow:   row["shallow"],
            retained:  row["retained"],
            heap_pct:  pct(row["retained"], heap_bytes),
            depth:     depth,
            idom_class: pcls,
        }
    });

    print(`  IDX       DEPTH  HEAP%   IDOM_CLASS`);
    for row in enriched_maps {
        let pcls = row["idom_class"];
        if pcls.len() > 40 { pcls = pcls.sub_string(0, 40); }
        print(`  [${row["idx"]}]  depth=${row["depth"]}  ${row["heap_pct"]}  ${pcls}`);
    }

    // OQL CANNOT DO THIS: retained-set breakdown of the biggest one
    let biggest = maps[0]["idx"];
    let rs = retained_set_summary(biggest);
    print(`\n  Biggest HashMap [${biggest}] retained set (top 5):`);
    let rs_shown = 0;
    for r in rs {
        if rs_shown >= 5 { break; }
        rs_shown += 1;
        print(`    ${r["class"]}  ${fmt_int(r["count"])} x  ${format_bytes(r["retained"])}`);
    }

    // oql_raw for metadata
    let raw = oql_raw("SELECT @objectId AS idx FROM java.lang.String LIMIT 1");
    if raw != () && raw.contains("row_count") {
        print(`\n  String OQL row_count: ${fmt_int(raw["row_count"])}  truncated=${raw["truncated"]}`);
    }
} else {
    print("  No java.util.HashMap instances found.");
}

// =============================================================================
section(11, "REFERENCE GRAPH ANALYSIS (requires --with-graph)");
// =============================================================================

if !is_graph_loaded() {
    print("  Graph not loaded. Re-run with --with-graph for this section.");
} else {
    let tops2 = top_objects(5);

    sub("Inbound reference classes for top objects");
    for obj in head(tops2, 3) {
        print(`  [${obj["idx"]}] ${obj["class"]}  (${format_bytes(obj["retained"])})`);
        let inb = incoming_by_class(obj["class"], 5);
        if inb.len() > 0 {
            for r in inb {
                print(`    <- ${r["class"]}  ${fmt_int(r["ref_count"])} refs  (${format_bytes(r["total_retained"])})`);
            }
        }
    }

    sub("Reference path between two large objects");
    if tops2.len() >= 2 {
        let a = tops2[0]["idx"];
        let b = tops2[1]["idx"];
        let path = reference_path(a, b, 12);
        if path.len() > 0 {
            print(`  Path [${a}]->[${b}] (${path.len() - 1} hops):`);
            let step = 0;
            for idx in path {
                step += 1;
                let obj_info = inspect(idx);
                print(`    ${step}. [${idx}] ${obj_info["class"]}  ${format_bytes(obj_info["retained_bytes"])}  (out=${obj_info["outbound_count"]}  in=${obj_info["inbound_count"]})`);
            }
        } else {
            print(`  No path from [${a}] to [${b}] within 12 hops.`);
        }
    }

    sub("Low-level outbound refs — CLOSURE enrichment");
    let big = tops2[0]["idx"];
    let out = outbound_refs(big);
    print(`  [${big}] has ${out.len()} outbound refs. Top 5 by retained:`);

    // CLOSURE + map + sort_by: enrich with retained info
    let out_enriched = map(out, |oidx| #{
        idx:      oidx,
        cls:      class_name(oidx),
        retained: retained(oidx),
    });
    out_enriched = sort_by(out_enriched, "retained");
    for o in head(out_enriched, 5) {
        print(`    -> [${o["idx"]}] ${o["cls"]}  ${format_bytes(o["retained"])}`);
    }

    // Inbound refs
    let inb_raw = inbound_refs(big);
    print(`  [${big}] has ${inb_raw.len()} inbound refs (first 5):`);
    let inb_shown = 0;
    for oidx in inb_raw {
        if inb_shown >= 5 { break; }
        inb_shown += 1;
        print(`    <- [${oidx}] ${class_name(oidx)}  ${format_bytes(retained(oidx))}`);
    }

    // incoming_by_class_limit
    let inb_cls = incoming_by_class_limit("byte[]", 5);
    if inb_cls.len() > 0 {
        print("\n  Who creates byte[] (top 5 by ref count):");
        for r in inb_cls {
            print(`    ${r["class"]}  ${fmt_int(r["ref_count"])} refs`);
        }
    }
}

// =============================================================================
section(12, "COMMON DOMINATOR CLUSTERING (LCA, impossible in OQL)");
// =============================================================================

let cluster_tops = top_objects(10);
if cluster_tops.len() >= 4 {
    print("  Pairwise LCA for top 4 objects:");
    let objs = head(cluster_tops, 4);
    let i = 0;
    while i < objs.len() {
        let j = i + 1;
        while j < objs.len() {
            let a = objs[i]["idx"];
            let b = objs[j]["idx"];
            let lca = common_dominator(a, b);
            if lca >= 0 {
                let lca_depth = dominator_depth(lca);
                print(`  [${a}] x [${b}] -> LCA [${lca}] ${class_name(lca)}  ${format_bytes(retained(lca))}  depth=${lca_depth}`);
            }
            j += 1;
        }
        i += 1;
    }
}

// =============================================================================
section(13, "WASTE ANALYSIS — over-allocated collections (CLOSURE filtering)");
// =============================================================================

let hmaps = instances_of("HashMap", 200);
// CLOSURE: filter over-allocated ones (shallow >> retained)
let over_alloc = filter(hmaps, |h| h["shallow"] > 0 && h["retained"] > 0 && h["shallow"] * 3 > h["retained"]);
over_alloc = sort_by(over_alloc, "shallow");

if over_alloc.len() > 0 {
    print(`  ${over_alloc.len()} HashMaps where shallow > retained/3:`);
    // CLOSURE + map: compute waste ratio as float
    let with_ratio = map(over_alloc, |h| #{
        idx:      h["idx"],
        class:    h["class"],
        shallow:  h["shallow"],
        retained: h["retained"],
        // float division — possible because floats are enabled
        waste_ratio: h["shallow"].to_float() / (h["retained"].to_float() + 1.0),
    });
    with_ratio = sort_by(with_ratio, "waste_ratio");

    for h in head(with_ratio, 8) {
        print(`  [${h["idx"]}] shallow=${format_bytes(h["shallow"])}  retained=${format_bytes(h["retained"])}  ratio=${h["waste_ratio"]}x`);
    }

    // CLOSURE + sum: total shallow bytes in over-allocated maps
    let total_shallow = sum(over_alloc, "shallow");
    print(`  Total shallow in over-allocated HashMaps: ${format_bytes(total_shallow)}`);
} else {
    print("  No obviously over-allocated HashMaps found.");
}

// =============================================================================
section(14, "REPORT DEEP-DIVE + NAMED VIEWS");
// =============================================================================

list_views();
let threads = report("threads");
if threads != () { print(`  Thread report loaded (${json_encode(threads).len()} bytes)`); }

let gc_roots = report("gc-roots");
if gc_roots != () && gc_roots.contains("gc_roots") {
    print(`  GC roots: ${fmt_int(gc_roots["gc_roots"])}`);
}

let strings_rpt = report("strings");
if strings_rpt != () && strings_rpt.contains("string_count") {
    print(`  String instances: ${fmt_int(strings_rpt["string_count"])}`);
}

// retained_set_filtered — show only array types in a suspect's subtree
if suspects.len() > 0 {
    let first_idx = suspects[0]["idx"];
    let arr_only  = retained_set_filtered(first_idx, "[]", 20);
    if arr_only != () {
        print(`  Array types in suspect [${first_idx}] retained set: ${arr_only["total_objects_in_subtree"]} total objects`);
    }
}

// =============================================================================
section(15, "JSON EXPORT — structured findings");
// =============================================================================

let top5 = [];
let ex_count = 0;
for s in suspects {
    if ex_count >= 5 { break; }
    ex_count += 1;
    top5.push(#{
        idx:      s["idx"],
        class:    s["class"],
        retained: s["retained"],
        heap_pct: pct(s["retained"], heap_bytes),
        // float ratio preserved exactly
        ratio:    s["ratio"],
    });
}

let export = #{
    dump:          info["dump"],
    heap_bytes:    heap_bytes,
    n_objects:     n_objects,
    outlier_count: outliers.len(),
    chain_count:   chains.len(),
    suspects:      top5,
    top_packages:  map(head(pkg_rows, 5), |r| #{
        package:  r["package"],
        retained: r["retained"],
        heap_pct: pct(r["retained"], heap_bytes),
    }),
};

let json_str  = json_encode(export);   // host function
let compact   = json(export);          // prelude alias
let roundtrip = json_decode(compact);
print(`  Export: ${fmt_int(json_str.len())} bytes JSON`);
print(`  Round-trip heap_bytes: ${format_bytes(roundtrip["heap_bytes"])}`);
pp(export);

print(`\n${SEP}\n  ANALYSIS COMPLETE — ${fmt_int(n_objects)} objects, heap ${format_bytes(heap_bytes)}\n${SEP}`);

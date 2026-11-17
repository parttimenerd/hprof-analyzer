// analysis_interactive.rhai — Beyond-OQL recipes (closures + floats)
// =============================================================================
// 20 self-contained recipes. Each can be pasted into the Rhai REPL or run
// as a standalone script. Closures and floats are fully enabled.
//
// Prelude provides: map, filter, sort_by, head, sum, count, print_table,
//   query, pp, walk_tree, investigate, json
//
// Usage: hprof-analyzer heap script rhai dump.hprof --file analysis_interactive.rhai
// =============================================================================

let info       = dump_info();
let heap_bytes = info["heap_bytes"];

// =============================================================================
print("-- RECIPE 1: QUICK TRIAGE WITH PERCENTAGES --");
// OQL cannot compute % of total heap.
// =============================================================================

let top5 = class_stats(5);
print(`  Heap: ${format_bytes(heap_bytes)}  across  ${fmt_int(info["objects"])} objects`);
// CLOSURE in map: format each row
let formatted = map(top5, |s| `  ${s["class"]}  ${format_bytes(s["total_retained"])}  ${pct(s["total_retained"], heap_bytes)}`);
for line in formatted { print(line); }

// =============================================================================
print("\n-- RECIPE 2: MULTI-LEVEL DOMINATOR CASCADE --");
// OQL selects flat lists. Scripts recursively drill through the dominator tree.
// =============================================================================

let suspects = accumulation_points(heap_bytes / 200, 8.0, 3);
for s in suspects {
    print(`  SUSPECT [${s["idx"]}] ${s["class"]}  ${format_bytes(s["retained"])}  ratio=${s["ratio"]}x`);

    let ch = big_drops(s["idx"], 3);
    for c in ch {
        print(`    L1 [${c["idx"]}] ${c["class"]}  ${format_bytes(c["retained"])}`);
        let gc_drops = big_drops(c["idx"], 2);
        for gc in gc_drops {
            print(`      L2 [${gc["idx"]}] ${gc["class"]}  ${format_bytes(gc["retained"])}`);
        }
    }
}

// =============================================================================
print("\n-- RECIPE 3: OUTLIER INSTANCES (statistical, impossible in OQL) --");
// OQL cannot compute per-class averages and flag deviating instances.
// Closures make this as concise as Lua.
// =============================================================================

let stats = class_stats(50);

// CLOSURE: filter to highly-skewed classes
let skewed = filter(stats, |s| {
    s["instances"] > 10
    && s["avg_retained"] > 0
    && s["max_retained"] > s["avg_retained"] * 20
});

for s in head(skewed, 5) {
    let ratio = s["max_retained"] / (s["avg_retained"] + 1);
    print(`  ${s["class"]}: ${fmt_int(s["instances"])} instances  avg=${format_bytes(s["avg_retained"])}  outlier=${format_bytes(s["max_retained"])}  (${ratio}x avg)`);
    let obj   = inspect(s["max_retained_idx"]);
    let depth = dominator_depth(s["max_retained_idx"]);
    let idom_idx = obj["idom"];
    let pcls = if idom_idx >= 0 { class_name(idom_idx) } else { "<root>" };
    print(`    -> [${s["max_retained_idx"]}] depth=${depth}  idom: ${pcls}`);
}

// =============================================================================
print("\n-- RECIPE 4: CROSS-CLASS RETENTION COMPARISON --");
// Closures keep the per-class loop concise.
// OQL must run separate queries per class; scripts compare them in one pass.
// =============================================================================

let targets = ["byte[]", "java.lang.String", "java.util.HashMap"];
let votes = #{};
for tgt in targets {
    let tot = class_total(tgt);
    print(`\n  ${tgt}: ${fmt_int(tot["instances"])} instances  ${format_bytes(tot["retained"])}  (${pct(tot["retained"], heap_bytes)})`);
    let retainers = who_retains(tgt, 4);
    for r in retainers {
        print(`    <- ${r["class"]}  holds ${fmt_int(r["instances"])}`);
        let cls = r["class"];
        if votes.contains(cls) { votes[cls] += 1; } else { votes[cls] = 1; }
    }
}
// CLOSURE + filter: find universal containers
let vote_arr = [];
for cls in votes.keys() { vote_arr.push(#{ class: cls, votes: votes[cls] }); }
let universal = filter(vote_arr, |v| v["votes"] >= 3);
for u in universal { print(`  *** UNIVERSAL CONTAINER: ${u["class"]} (${u["votes"]}/${targets.len()} types)`); }

// =============================================================================
print("\n-- RECIPE 5: INVESTIGATE (prelude one-call wrapper) --");
// investigate() finds accumulation points, walks paths, shows drops.
// =============================================================================

investigate(heap_bytes / 100, 5);

// =============================================================================
print("\n-- RECIPE 6: PACKAGE-LEVEL MEMORY ROLLUP --");
// OQL cannot GROUP BY derived fields. Scripts extract the package prefix
// and use closure-based aggregation — now as clean as Lua.
// =============================================================================

let full_hist = histogram_limit(300);
let pkg_retained = #{};
let pkg_objects  = #{};

for h in full_hist {
    let cls     = h["class"];
    let dot_pos = cls.index_of(".");
    let top_pkg = if dot_pos >= 0 { cls.sub_string(0, dot_pos) } else { "(none)" };

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
    pkg_rows.push(#{ package: pkg, retained: pkg_retained[pkg], objects: pkg_objects[pkg] });
}
pkg_rows = sort_by(pkg_rows, "retained");

for row in head(pkg_rows, 10) {
    let pkg = row["package"];
    if pkg.len() > 20 { pkg = pkg.sub_string(0, 20); }
    print(`  ${pkg}  ${format_bytes(row["retained"])}  ${pct(row["retained"], heap_bytes)}`);
}

// =============================================================================
print("\n-- RECIPE 7: DOMINATOR DEPTH HISTOGRAM --");
// OQL has no depth concept. Closures + map make this a one-liner enrichment.
// =============================================================================

let tops = top_objects(200);
// CLOSURE + map: enrich with depth
let depths = map(tops, |obj| dominator_depth(obj["idx"]));
// Build histogram
let d_hist = #{};
for d in depths {
    let capped = if d > 12 { 12 } else { d };
    let k = `${capped}`;
    if d_hist.contains(k) { d_hist[k] += 1; } else { d_hist[k] = 1; }
}
let d_rows = [];
for k in d_hist.keys() { d_rows.push(#{ depth: parse_int(k), count: d_hist[k] }); }
d_rows = sort_by(d_rows, "depth", false);
for row in d_rows {
    let bar = "";
    let bl = row["count"] / 3;
    while bl > 0 { bar += "|"; bl -= 1; }
    print(`  depth ${row["depth"]}  ${fmt_int(row["count"])} objects  ${bar}`);
}

// =============================================================================
print("\n-- RECIPE 8: RETAINED SET DIFFING BETWEEN TWO SUSPECTS --");
// OQL cannot enumerate or compare retained sets across two objects.
// =============================================================================

let s2 = accumulation_points(heap_bytes / 50, 5.0, 2);
if s2.len() >= 2 {
    // CLOSURE + map: build class->retained map from retained_set_summary
    fn rs_map(idx) {
        let m = #{};
        let rs = retained_set_summary(idx);
        for r in rs { m[r["class"]] = r["retained"]; }
        m
    }

    let rs_a = rs_map(s2[0]["idx"]);
    let rs_b = rs_map(s2[1]["idx"]);
    print(`  [${s2[0]["idx"]}] ${s2[0]["class"]}  vs  [${s2[1]["idx"]}] ${s2[1]["class"]}`);

    for cls in rs_a.keys() {
        let ret_a = rs_a[cls];
        let ret_b = if rs_b.contains(cls) { rs_b[cls] } else { 0 };
        if ret_b == 0 && ret_a > 1048576 {
            print(`  ONLY-A: ${cls}  ${format_bytes(ret_a)}`);
        } else if ret_a > 0 && ret_b > 0 {
            let diff = ret_a - ret_b;
            if diff > 1048576 || diff < -1048576 {
                print(`  DIFF  : ${cls}  A=${format_bytes(ret_a)}  B=${format_bytes(ret_b)}  delta=${format_bytes(diff)}`);
            }
        }
    }
} else {
    print("  Need at least 2 suspects for diffing.");
}

// =============================================================================
print("\n-- RECIPE 9: OQL + ENRICHMENT PIPELINE --");
// Run OQL for selection, closures for enrichment (depth, parent class, %).
// The enrichment is completely outside OQL's capabilities.
// =============================================================================

let rows = oql("SELECT @objectId AS idx, @retainedHeapSize AS ret FROM java.util.ArrayList ORDER BY ret DESC LIMIT 6");

// CLOSURE + map: enrich each result with dominator context
let enriched = map(rows, |row| #{
    idx:      row["idx"],
    ret:      row["ret"],
    heap_pct: pct(row["ret"], heap_bytes),
    depth:    dominator_depth(row["idx"]),
    idom_cls: if idom(row["idx"]) >= 0 { class_name(idom(row["idx"])) } else { "<root>" },
});

for row in enriched {
    let pcls = row["idom_cls"];
    if pcls.len() > 36 { pcls = pcls.sub_string(0, 36); }
    print(`  [${row["idx"]}]  depth=${row["depth"]}  ${row["heap_pct"]}  idom: ${pcls}`);
}

// =============================================================================
print("\n-- RECIPE 10: COLLECTION WASTE VIA FLOAT RATIO --");
// Floats are now available. Compute waste ratio as a proper float.
// OQL cannot compute shallow/retained ratios.
// =============================================================================

let lists = instances_of("ArrayList", 300);

// CLOSURE with float division — available because no_float is removed
let bloated = filter(lists, |o| o["shallow"] > 512 && o["retained"] > 0 && o["shallow"] > o["retained"] * 2);
bloated = sort_by(bloated, "shallow");

// CLOSURE + map: compute float ratio
let with_ratio = map(bloated, |o| #{
    idx:      o["idx"],
    shallow:  o["shallow"],
    retained: o["retained"],
    ratio:    o["shallow"].to_float() / (o["retained"].to_float() + 1.0),
});
// CLOSURE + filter: only really bad ones (ratio > 5.0)
let very_bad = filter(with_ratio, |o| o["ratio"] > 5.0);
print(`  ${bloated.len()} ArrayLists with shallow>2x retained, ${very_bad.len()} with ratio>5x:`);
for o in head(very_bad, 5) {
    print(`  [${o["idx"]}]  shallow=${format_bytes(o["shallow"])}  retained=${format_bytes(o["retained"])}  ratio=${o["ratio"]}x`);
}

// =============================================================================
print("\n-- RECIPE 11: CHAIN DETECTION + CLOSURE ENRICHMENT --");
// =============================================================================

let chains = detect_chains(4, 15);
// CLOSURE + map: enrich with float ratio
let scored = map(chains, |c| #{
    class:       c["class"],
    length:      c["length"],
    total:       c["total_retained"],
    heap_pct:    pct(c["total_retained"], heap_bytes),
    per_node:    c["total_retained"] / (c["length"] + 1),
});
scored = sort_by(scored, "total");

if scored.len() > 0 {
    print(`  ${scored.len()} chains found:`);
    for c in head(scored, 8) {
        print(`  len=${c["length"]}  ${c["class"]}  ${format_bytes(c["total"])}  ${c["heap_pct"]}`);
    }
}

// =============================================================================
print("\n-- RECIPE 12: OWNERSHIP CHAIN RECONSTRUCTION --");
// Walk the idom chain from an object to the root, annotating each level.
// OQL has no idom traversal.
// =============================================================================

let seed = top_objects(1)[0];
print(`  Ownership chain from [${seed["idx"]}] ${seed["class"]}:`);
let path = dominator_path(seed["idx"]);
let level = 0;
for p in path {
    level += 1;
    let indent = "";
    let d = level;
    while d > 0 { indent += "  "; d -= 1; }
    print(`  ${indent}[${p["idx"]}] ${p["class"]}  ${format_bytes(p["retained"])}  (${pct(p["retained"], heap_bytes)})`);
}

// =============================================================================
print("\n-- RECIPE 13: REFERENCE GRAPH — INBOUND ANALYSIS --");
// OQL cannot traverse reference edges. Scripts + closures make this clean.
// =============================================================================

if is_graph_loaded() {
    let inbound = incoming_by_class_limit("byte[]", 10);
    print("  Classes holding references into byte[] arrays:");
    // CLOSURE + sort_by: rank by total_retained
    inbound = sort_by(inbound, "total_retained");
    for r in inbound {
        print(`  ← ${r["class"]}  ${fmt_int(r["ref_count"])} refs  ${format_bytes(r["total_retained"])}`);
    }

    // Enrich outbound refs with closures
    let big = top_objects(1)[0];
    let out = outbound_refs(big["idx"]);
    // CLOSURE + map + sort_by
    let out_rich = map(out, |oidx| #{ idx: oidx, cls: class_name(oidx), ret: retained(oidx) });
    out_rich = sort_by(out_rich, "ret");
    print(`\n  [${big["idx"]}] ${big["class"]} top 5 outbound by retained:`);
    for o in head(out_rich, 5) {
        print(`  -> [${o["idx"]}] ${o["cls"]}  ${format_bytes(o["ret"])}`);
    }
} else {
    print("  Skip (requires --with-graph)");
}

// =============================================================================
print("\n-- RECIPE 14: FULL RETAINED SET EXPLORATION --");
// retained_set_filtered enumerates objects in a subtree with class filter.
// OQL cannot enumerate subtrees.
// =============================================================================

let big2 = top_objects(1)[0];
let arr_set = retained_set_filtered(big2["idx"], "[]", 30);
if arr_set != () && arr_set.contains("total_objects_in_subtree") {
    print(`  [${big2["idx"]}] retains ${fmt_int(arr_set["total_objects_in_subtree"])} objects total`);
    let objects = arr_set["objects"];
    if objects != () {
        for o in head(objects, 6) {
            print(`  depth=${o["depth"]}  [${o["index"]}] ${o["class"]}  ${format_bytes(o["shallow_bytes"])}`);
        }
    }
}

// =============================================================================
print("\n-- RECIPE 15: CLASS HIERARCHY RETENTION (CLOSURE filter) --");
// Compare all Map/Set implementations. OQL queries one class at a time;
// scripts aggregate across all matching class names in one pass.
// =============================================================================

let all_stats = class_stats(200);
// CLOSURE: filter to collection types
let coll_stats = filter(all_stats, |s| {
    let cls = s["class"];
    cls.contains("Map") || cls.contains("Set") || cls.contains("List")
});
coll_stats = sort_by(coll_stats, "total_retained");
// CLOSURE + sum: total retained in collection classes
let coll_total = sum(coll_stats, "total_retained");
print(`  ${coll_stats.len()} collection classes hold ${format_bytes(coll_total)} (${pct(coll_total, heap_bytes)} of heap):`);
for s in head(coll_stats, 8) {
    print(`  ${s["class"]}  ${fmt_int(s["instances"])} x  avg=${format_bytes(s["avg_retained"])}  total=${format_bytes(s["total_retained"])}`);
}

// =============================================================================
print("\n-- RECIPE 16: COMMON DOMINATOR CLUSTERING --");
// Compute pairwise LCAs for top objects, cluster them by shared root.
// OQL cannot compute LCAs.
// =============================================================================

let ctops = head(top_objects(8), 8);
let lca_counts = #{};
let i = 0;
while i < ctops.len() {
    let j = i + 1;
    while j < ctops.len() {
        let lca = common_dominator(ctops[i]["idx"], ctops[j]["idx"]);
        if lca >= 0 {
            let lcls = class_name(lca);
            if lca_counts.contains(lcls) { lca_counts[lcls] += 1; }
            else { lca_counts[lcls] = 1; }
        }
        j += 1;
    }
    i += 1;
}
let lca_rows = [];
for cls in lca_counts.keys() { lca_rows.push(#{ class: cls, count: lca_counts[cls] }); }
lca_rows = sort_by(lca_rows, "count");
print("  Most common LCA classes (shared retention roots):");
for row in head(lca_rows, 5) {
    print(`  %-44s ${row["count"]} pairs share this root`);
}

// =============================================================================
print("\n-- RECIPE 17: SUBTREE WALK + CLASS AGGREGATION --");
// Walk the dominator subtree and aggregate retained by class.
// OQL cannot enumerate or aggregate within a subtree.
// =============================================================================

let big3 = top_objects(1)[0];
// walk_tree: prelude visual output
walk_tree(big3["idx"], 2, 8);

let subtree = walk_subtree(big3["idx"], 4, 200);
// CLOSURE + manual grouping (could use sort_by on accumulated array)
let sub_cls = #{};
for node in subtree {
    let cls = node["class"];
    if sub_cls.contains(cls) { sub_cls[cls] += node["retained"]; }
    else { sub_cls[cls] = node["retained"]; }
}
let sub_rows = [];
for cls in sub_cls.keys() { sub_rows.push(#{ class: cls, retained: sub_cls[cls] }); }
sub_rows = sort_by(sub_rows, "retained");
print(`\n  Top 5 classes in subtree of [${big3["idx"]}]:`);
for row in head(sub_rows, 5) {
    print(`  ${row["class"]}  ${format_bytes(row["retained"])}`);
}

// =============================================================================
print("\n-- RECIPE 18: THREAD-TO-OBJECT ATTRIBUTION --");
// Combine report sections with heap analysis.
// =============================================================================

let threads_rpt = report("threads");
if threads_rpt != () {
    print("  Thread report available — cross-referencing heap Thread objects...");
    let thread_objs = instances_of("java.lang.Thread", 20);
    print(`  Found ${thread_objs.len()} Thread instances`);
    for t in head(thread_objs, 3) {
        let rs = head(retained_set_summary(t["idx"]), 3);
        print(`  Thread [${t["idx"]}]  retained=${format_bytes(t["retained"])}:`);
        for r in rs {
            print(`    ${r["class"]}  ${fmt_int(r["count"])} x  ${format_bytes(r["retained"])}`);
        }
    }
}

// =============================================================================
print("\n-- RECIPE 19: NAMED VIEWS + OQL_RAW METADATA --");
// =============================================================================

list_views();
let raw = oql_raw("SELECT @objectId AS idx FROM java.lang.String LIMIT 1");
if raw != () && raw.contains("row_count") {
    print(`  String count from oql_raw: ${fmt_int(raw["row_count"])}  truncated=${raw["truncated"]}`);
}

// browse_tree from root (browse_tree_default uses depth=3 width=10)
let tree = browse_tree_default(-1);
if tree != () { print(`  Dominator tree root children: ${tree["children"].len()}`); }

// json_decode round-trip
let blob = json_encode(#{ test: 42, ratio: 3.14 });
let back = json_decode(blob);
print(`  json round-trip: test=${back["test"]}  ratio=${back["ratio"]}`);

// =============================================================================
print("\n-- RECIPE 20: FULL AUTOMATED TRIAGE FUNCTION (CLOSURES THROUGHOUT) --");
// Assembles findings from multiple analyses using closures at every step.
// =============================================================================

fn full_triage() {
    let heap = dump_info()["heap_bytes"];
    let findings = [];

    // CLOSURE: find top class
    let top_cls = head(class_stats(1), 1);
    if top_cls.len() > 0 {
        let tc = top_cls[0];
        findings.push(`Top class: ${tc["class"]}  ${format_bytes(tc["total_retained"])}  (${pct(tc["total_retained"], heap)})`);
    }

    // CLOSURE: biggest accumulation point
    let pts = accumulation_points(heap / 50, 5.0, 1);
    if pts.len() > 0 {
        let p = pts[0];
        // float ratio preserved
        findings.push(`Biggest suspect: [${p["idx"]}] ${p["class"]}  ${format_bytes(p["retained"])}  ratio=${p["ratio"]}x`);
    }

    // CLOSURE: chain summary
    let ch = detect_chains(5, 3);
    if ch.len() > 0 {
        findings.push(`Longest chain: ${ch[0]["length"]} x ${ch[0]["class"]}  (${format_bytes(ch[0]["total_retained"])})`);
    }

    // CLOSURE + sum: total collection bytes
    let all_st = class_stats(200);
    let coll    = filter(all_st, |s| s["class"].contains("Map") || s["class"].contains("List"));
    let coll_total = sum(coll, "total_retained");
    findings.push(`Collection classes: ${coll.len()} types  ${format_bytes(coll_total)}  (${pct(coll_total, heap)})`);

    // Strings
    let str_tot = class_total("java.lang.String");
    findings.push(`Strings: ${fmt_int(str_tot["instances"])} x  ${format_bytes(str_tot["retained"])}  (${pct(str_tot["retained"], heap)})`);

    print("\n  ╔══ TRIAGE SUMMARY ══╗");
    let n = 1;
    for f in findings { print(`  ${n}. ${f}`); n += 1; }
    print("  ╚════════════════════╝");

    pp(#{ heap_bytes: heap, finding_count: findings.len(), findings: findings });
}

full_triage();

print("\n=== ALL 20 RECIPES COMPLETE ===");

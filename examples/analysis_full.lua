-- analysis_full.lua — Beyond-OQL heap dump investigation
-- ═══════════════════════════════════════════════════════════════════════════
-- Demonstrates analysis impossible in OQL alone:
--   multi-step tree traversal, statistical outlier detection,
--   cross-class correlation, graph-structure metrics, package rollups,
--   OQL result enrichment, and dominator-aware waste detection.
--
-- Usage:
--   hprof-analyzer heap script lua dump.hprof --file analysis_full.lua
--   hprof-analyzer heap script lua dump.hprof --file analysis_full.lua --with-graph
-- ═══════════════════════════════════════════════════════════════════════════

local SEP  = string.rep("═", 72)
local SEP2 = string.rep("─", 60)

local function section(n, title)
    print(string.format("\n%s\n  §%d  %s\n%s", SEP, n, title, SEP))
end
local function sub(title)
    print(string.format("\n  %s\n  %s\n  %s", SEP2, title, SEP2))
end

-- ═══════════════════════════════════════════════════════════════════════════
section(1, "TRIAGE — heap overview")
-- OQL cannot produce heap totals or cross-field stats.
-- ═══════════════════════════════════════════════════════════════════════════

local info       = dump_info()
local heap_bytes = info.heap_bytes
local n_objects  = info.objects

print(string.format("  Dump    : %s", info.dump))
print(string.format("  Objects : %s", fmt_int(n_objects)))
print(string.format("  Classes : %s", fmt_int(info.classes)))
print(string.format("  Heap    : %s  (object_count=%s)", format_bytes(heap_bytes), fmt_int(object_count())))
print(string.format("  Graph   : %s", info.graph and "loaded" or "not loaded (use --with-graph)"))

-- ═══════════════════════════════════════════════════════════════════════════
section(2, "REPORT HIGHLIGHTS — triage, leaks, waste")
-- OQL has no concept of pre-computed reports or leak heuristics.
-- ═══════════════════════════════════════════════════════════════════════════

local secs = report_sections()
print("  Available report sections: " .. #secs)

local triage = report("triage")
if triage and triage.verdict then
    print(string.format("  Verdict    : %s", triage.verdict or "n/a"))
end
if triage and triage.primary_suspect then
    print(string.format("  Primary suspect : %s", triage.primary_suspect or "n/a"))
end

local leaks = report("leaks")
if leaks and type(leaks) == "table" then
    local n = 0
    for _ in pairs(leaks) do n = n + 1 end
    print(string.format("  Leak section keys: %d", n))
end

local waste = report("waste")
if waste and waste.waste_summary then
    local ws = waste.waste_summary
    if ws.total_wasted_bytes then
        print(string.format("  Estimated waste : %s", format_bytes(ws.total_wasted_bytes)))
    end
end

-- ═══════════════════════════════════════════════════════════════════════════
section(3, "CLASS HISTOGRAM — heap% per class (OQL can't compute %%)")
-- OQL can ORDER BY retainedHeapSize but cannot compute % of total heap.
-- ═══════════════════════════════════════════════════════════════════════════

local hist = histogram(20)
print(string.format("  %-50s %8s %8s %8s", "CLASS", "COUNT", "RETAINED", "HEAP%"))
print(string.rep("-", 80))
for i, h in ipairs(hist) do
    print(string.format("  %2d. %-46s %8s %8s %6s",
        i,
        string.sub(h.class, 1, 46),
        fmt_int(h.instances),
        format_bytes(h.retained_bytes),
        pct(h.retained_bytes, heap_bytes)))
end

-- ═══════════════════════════════════════════════════════════════════════════
section(4, "STATISTICAL OUTLIER DETECTION (OQL can't compute per-class statistics)")
-- OQL can find the biggest instance of a class, but can't compute
-- the average across all instances or flag ones that deviate.
-- ═══════════════════════════════════════════════════════════════════════════

local stats = class_stats(100)

-- Classes where the single biggest object is >10x the per-class average.
-- This signals one bloated outlier hidden among many small ones.
local outliers = filter(stats, function(s)
    return s.instances > 5
       and s.avg_retained > 0
       and s.max_retained > s.avg_retained * 10
end)

if #outliers == 0 then
    print("  No strong outliers found.")
else
    print(string.format("  %-48s %6s %10s %10s %6s",
        "CLASS", "COUNT", "AVG", "MAX", "RATIO"))
    print(string.rep("-", 86))
    for _, s in ipairs(head(outliers, 10)) do
        local ratio = s.max_retained / math.max(s.avg_retained, 1)
        print(string.format("  %-48s %6s %10s %10s %5.1fx",
            string.sub(s.class, 1, 48),
            fmt_int(s.instances),
            format_bytes(s.avg_retained),
            format_bytes(s.max_retained),
            ratio))
        -- OQL cannot do this next step: inspect the outlier object itself
        local obj = inspect(s.max_retained_idx)
        local depth = dominator_depth(s.max_retained_idx)
        print(string.format("    → outlier [%d] depth=%d  idom=[%d] %s",
            s.max_retained_idx, depth,
            obj.idom, class_name(obj.idom >= 0 and obj.idom or 0)))
    end
end

-- ═══════════════════════════════════════════════════════════════════════════
section(5, "ACCUMULATION POINT CASCADE (multi-level recursive tree traversal)")
-- OQL selects objects but cannot recursively walk the dominator tree,
-- compute multi-level drops, or classify what each subtree consists of.
-- ═══════════════════════════════════════════════════════════════════════════

local threshold = math.max(heap_bytes / 100, 1048576) -- 1% of heap, min 1 MB
local suspects  = accumulation_points(threshold, 10.0, 10)

if #suspects == 0 then
    print("  No accumulation points above threshold.")
else
    for i, s in ipairs(suspects) do
        print(string.format("\n  [%d/%d] [idx=%d] %s", i, #suspects, s.idx, s.class))
        print(string.format("    shallow=%s  retained=%s  ratio=%.1fx  heap%%=%s",
            format_bytes(s.shallow), format_bytes(s.retained),
            s.ratio, pct(s.retained, heap_bytes)))

        -- OQL CANNOT DO THIS: walk the dominator path upward
        local path = dominator_path(s.idx)
        if #path > 1 then
            print("    Retained-by chain:")
            for j, p in ipairs(path) do
                local indent = string.rep("  ", j)
                print(string.format("    %s→ [%d] %s (%s)",
                    indent, p.idx, p.class, format_bytes(p.retained)))
            end
        end

        -- OQL CANNOT DO THIS: show children with retained-size drops
        local drops = big_drops(s.idx, 4)
        if #drops > 0 then
            print("    Biggest children (by retained):")
            for _, d in ipairs(drops) do
                print(string.format("    └─ [%d] %-40s %s  (drop→grandchild: %s)",
                    d.idx, d.class, format_bytes(d.retained), format_bytes(d.drop)))
            end
        end

        -- OQL CANNOT DO THIS: class breakdown of the entire retained set
        local rs = retained_set_summary(s.idx)
        if #rs > 0 then
            print("    Retained set composition (top 4):")
            for _, r in ipairs(head(rs, 4)) do
                print(string.format("    │  %-40s %5s instances  %s",
                    r.class, fmt_int(r.count), format_bytes(r.retained)))
            end
        end

        -- OQL CANNOT DO THIS: go one level deeper into the biggest child
        if #drops > 0 then
            local child_idx = drops[1].idx
            local child_drops = big_drops(child_idx, 3)
            if #child_drops > 0 then
                print(string.format("    Drilling into biggest child [%d] %s:",
                    child_idx, drops[1].class))
                for _, cd in ipairs(child_drops) do
                    print(string.format("      └─ [%d] %s  %s",
                        cd.idx, cd.class, format_bytes(cd.retained)))
                end
            end
        end
    end
end

-- ═══════════════════════════════════════════════════════════════════════════
section(6, "RETENTION CHAIN COMPARISON (cross-class analysis OQL can't express)")
-- OQL can query instances of one class. It cannot compare which classes
-- are the top retainers of byte[], String, and HashMap simultaneously
-- and then rank those retainer classes across all three target types.
-- ═══════════════════════════════════════════════════════════════════════════

local targets = {"byte[]", "char[]", "java.lang.String", "java.util.HashMap"}
local retainer_votes = {}  -- class → number of target types it retains

for _, tgt in ipairs(targets) do
    local tot = class_total(tgt)
    print(string.format("\n  %s: %s instances, %s retained (%s of heap)",
        tgt, fmt_int(tot.instances), format_bytes(tot.retained),
        pct(tot.retained, heap_bytes)))

    local retainers = who_retains(tgt, 5)
    for _, r in ipairs(retainers) do
        print(string.format("    ← %-44s holds %5s  (%s)",
            r.class, fmt_int(r.instances), format_bytes(r.total_retained)))
        retainer_votes[r.class] = (retainer_votes[r.class] or 0) + 1
    end
end

-- OQL CANNOT DO THIS: find classes that retain ALL target types
local universal = {}
for cls, votes in pairs(retainer_votes) do
    if votes >= 3 then
        universal[#universal + 1] = {class = cls, votes = votes}
    end
end
if #universal > 0 then
    print("\n  Classes retaining 3+ target types (universal containers):")
    for _, u in ipairs(sort_by(universal, "votes", true)) do
        print(string.format("    %s  (retains %d of %d target types)", u.class, u.votes, #targets))
    end
end

-- ═══════════════════════════════════════════════════════════════════════════
section(7, "DOMINATOR TREE STRUCTURE ANALYSIS (graph metrics OQL can't compute)")
-- OQL cannot compute fan-out, depth, or structural properties of the
-- dominator tree — it has no concept of the retention hierarchy.
-- ═══════════════════════════════════════════════════════════════════════════

sub("Fan-out distribution of root's direct children")
local root_children = children(top_objects(1)[1].idx)
local fan_out_buckets = {["1"]=0, ["2-5"]=0, ["6-20"]=0, [">20"]=0}
local total_children_ret = 0

for _, cidx in ipairs(head(root_children, 200)) do
    local grandkids = children(cidx)
    local n = #grandkids
    total_children_ret = total_children_ret + retained(cidx)
    if     n == 1             then fan_out_buckets["1"]    = fan_out_buckets["1"]    + 1
    elseif n <= 5             then fan_out_buckets["2-5"]  = fan_out_buckets["2-5"]  + 1
    elseif n <= 20            then fan_out_buckets["6-20"] = fan_out_buckets["6-20"] + 1
    else                           fan_out_buckets[">20"]  = fan_out_buckets[">20"]  + 1
    end
end

local n_sampled = #head(root_children, 200)
for bucket, cnt in pairs(fan_out_buckets) do
    print(string.format("  fan-out %-6s : %4d nodes  (%s)",
        bucket, cnt, pct(cnt, n_sampled)))
end

sub("Dominator depth distribution for top 500 objects")
local tops = top_objects(500)
local depth_hist = {}
for _, obj in ipairs(tops) do
    local d = math.min(dominator_depth(obj.idx), 15)  -- cap at 15
    local key = tostring(d)
    depth_hist[key] = (depth_hist[key] or 0) + 1
end

local depth_rows = {}
for d_str, cnt in pairs(depth_hist) do
    depth_rows[#depth_rows + 1] = {depth = tonumber(d_str), count = cnt}
end
depth_rows = sort_by(depth_rows, "depth", false)
print(string.format("  %-8s %6s %s", "DEPTH", "OBJECTS", "BAR"))
for _, row in ipairs(depth_rows) do
    local bar = string.rep("█", math.floor(row.count / 5))
    print(string.format("  depth %2d  %6d  %s", row.depth, row.count, bar))
end

-- ═══════════════════════════════════════════════════════════════════════════
section(8, "PACKAGE-LEVEL MEMORY ROLLUP (custom grouping OQL can't do)")
-- OQL has no GROUP BY on derived fields. Scripts can extract the package
-- prefix from the class name and aggregate across all classes.
-- ═══════════════════════════════════════════════════════════════════════════

local pkg_retained = {}
local pkg_objects  = {}

-- Walk ALL classes via histogram (avoids scanning all objects naively)
local full_hist = histogram(500)
for _, h in ipairs(full_hist) do
    -- Extract package: everything up to the last '.'
    local cls = h.class
    local dot = 0
    for pos = 1, #cls do
        if cls:sub(pos, pos) == "." then dot = pos end
    end
    local pkg = dot > 0 and cls:sub(1, dot - 1) or "(no package)"
    -- Roll up to top-level (first component)
    local first_dot = cls:find("%.")
    local top_pkg = first_dot and cls:sub(1, first_dot - 1) or pkg

    pkg_retained[top_pkg] = (pkg_retained[top_pkg] or 0) + h.retained_bytes
    pkg_objects[top_pkg]  = (pkg_objects[top_pkg]  or 0) + h.instances
end

local pkg_rows = {}
for pkg, ret in pairs(pkg_retained) do
    pkg_rows[#pkg_rows + 1] = {
        package  = pkg,
        retained = ret,
        objects  = pkg_objects[pkg] or 0,
    }
end
pkg_rows = sort_by(pkg_rows, "retained", true)

print(string.format("  %-20s %8s %10s %6s", "PACKAGE", "OBJECTS", "RETAINED", "HEAP%"))
print(string.rep("-", 50))
for _, row in ipairs(head(pkg_rows, 15)) do
    print(string.format("  %-20s %8s %10s %6s",
        string.sub(row.package, 1, 20),
        fmt_int(row.objects),
        format_bytes(row.retained),
        pct(row.retained, heap_bytes)))
end

-- ═══════════════════════════════════════════════════════════════════════════
section(9, "SAME-CLASS CHAIN DETECTION + CORRELATION")
-- OQL can find instances by class but can't detect dominator-tree chains
-- (A dominates B dominates C, all same class) or correlate chain length
-- with retained size to identify which chains are the worst.
-- ═══════════════════════════════════════════════════════════════════════════

local chains = detect_chains(3, 20)
if #chains == 0 then
    print("  No chains of length ≥ 3 found.")
else
    -- OQL CANNOT DO THIS: correlate chain length with retained-to-shallow ratio
    local enriched = map(chains, function(c)
        local sh = shallow(c.idx)
        return {
            class          = c.class,
            length         = c.length,
            total_retained = c.total_retained,
            head_retained  = retained(c.idx),
            shallow_head   = sh,
            ratio          = sh > 0 and (retained(c.idx) / sh) or 0,
        }
    end)
    enriched = sort_by(enriched, "total_retained", true)

    print(string.format("  %-44s %6s %10s %8s",
        "CLASS", "LEN", "TOTAL_RET", "HDR_RATIO"))
    print(string.rep("-", 74))
    for _, c in ipairs(head(enriched, 10)) do
        print(string.format("  %-44s %6d %10s %7.1fx",
            string.sub(c.class, 1, 44),
            c.length, format_bytes(c.total_retained), c.ratio))
    end
end

-- ═══════════════════════════════════════════════════════════════════════════
section(10, "OQL ENRICHMENT PIPELINE (hybrid: OQL + dominator context)")
-- OQL can find the biggest HashMaps. It cannot then cross-reference them
-- with dominator depth, their parent class, or their retained-set breakdown.
-- ═══════════════════════════════════════════════════════════════════════════

local maps = oql([[
    SELECT @objectId AS idx, @usedHeapSize AS shallow, @retainedHeapSize AS retained
    FROM   java.util.HashMap
    ORDER  BY retained DESC
    LIMIT  8
]])

if #maps > 0 then
    print(string.format("  %-8s %-40s %10s %10s %6s %5s",
        "IDX", "IDOM_CLASS", "SHALLOW", "RETAINED", "HEAP%", "DEPTH"))
    print(string.rep("-", 82))

    for _, row in ipairs(maps) do
        local idx    = row.idx
        local parent = idom(idx)
        local pcls   = parent >= 0 and class_name(parent) or "<root>"
        local depth  = dominator_depth(idx)
        -- OQL CANNOT DO THIS: cross-reference with dominator info
        print(string.format("  [%6d] %-40s %10s %10s %6s %5d",
            idx,
            string.sub(pcls, 1, 40),
            format_bytes(shallow(idx)),
            format_bytes(retained(idx)),
            pct(retained(idx), heap_bytes),
            depth))
    end

    -- OQL CANNOT DO THIS: retained-set composition of the biggest one
    if #maps > 0 then
        local biggest = maps[1].idx
        local rs = retained_set_summary(biggest)
        print(string.format("\n  Biggest HashMap [%d] retained set (top 5 classes):", biggest))
        for _, r in ipairs(head(rs, 5)) do
            print(string.format("    %-40s %6s instances  %s",
                r.class, fmt_int(r.count), format_bytes(r.retained)))
        end
    end
else
    print("  No java.util.HashMap instances found.")
end

-- Also demonstrate oql_raw for metadata inspection
local raw = oql_raw("SELECT @objectId AS idx FROM java.lang.String LIMIT 1")
if raw and raw.row_count then
    print(string.format("\n  String OQL: %s total rows, truncated=%s",
        fmt_int(raw.row_count), tostring(raw.truncated or false)))
end

-- ═══════════════════════════════════════════════════════════════════════════
section(11, "REFERENCE GRAPH ANALYSIS (requires --with-graph)")
-- OQL cannot traverse reference edges or find reference paths.
-- ═══════════════════════════════════════════════════════════════════════════

if not is_graph_loaded() then
    print("  Graph not loaded. Re-run with --with-graph for this section.")
else
    local tops2 = top_objects(5)

    sub("Who creates/references the top objects? (inbound reference classes)")
    for _, obj in ipairs(head(tops2, 3)) do
        print(string.format("  [%d] %s  (%s)", obj.idx, obj.class, format_bytes(obj.retained)))
        local inb = incoming_by_class(obj.class, 5)
        if #inb > 0 then
            for _, r in ipairs(inb) do
                print(string.format("    ← %-40s %4s refs  (%s total retained)",
                    r.class, fmt_int(r.ref_count), format_bytes(r.total_retained)))
            end
        else
            print("    (no inbound references indexed)")
        end
    end

    sub("Reference path between two large objects")
    if #tops2 >= 2 then
        local a, b = tops2[1].idx, tops2[2].idx
        local path = reference_path(a, b, 12)
        if #path > 0 then
            print(string.format("  Path [%d]→[%d] (%d hops):", a, b, #path - 1))
            for step, idx in ipairs(path) do
                local obj_info = inspect(idx)
                print(string.format("    %2d. [%d] %s  %s  (out=%d in=%d)",
                    step, idx, obj_info.class,
                    format_bytes(obj_info.retained_bytes),
                    obj_info.outbound_count or 0,
                    obj_info.inbound_count or 0))
            end
        else
            print(string.format("  No path from [%d] to [%d] within 12 hops.", a, b))
        end
    end

    sub("Low-level outbound refs of biggest object")
    local big = tops2[1].idx
    local out = outbound_refs(big)
    print(string.format("  [%d] has %d outbound refs. Top 5 by retained:", big, #out))
    local out_enriched = map(out, function(oidx)
        return {idx = oidx, retained = retained(oidx), class = class_name(oidx)}
    end)
    out_enriched = sort_by(out_enriched, "retained", true)
    for _, o in ipairs(head(out_enriched, 5)) do
        print(string.format("    → [%d] %-40s %s", o.idx, o.class, format_bytes(o.retained)))
    end

    sub("Inbound refs of biggest object")
    local inb = inbound_refs(big)
    print(string.format("  [%d] has %d inbound refs (first 5):", big, #inb))
    for _, oidx in ipairs(head(inb, 5)) do
        print(string.format("    ← [%d] %-40s %s", oidx, class_name(oidx), format_bytes(retained(oidx))))
    end
end

-- ═══════════════════════════════════════════════════════════════════════════
section(12, "COMMON DOMINATOR CLUSTERING (set-theory OQL can't do)")
-- OQL cannot compute LCAs or cluster objects by shared dominator.
-- ═══════════════════════════════════════════════════════════════════════════

local cluster_tops = top_objects(10)
if #cluster_tops >= 4 then
    print("  Pairwise common dominators for top 4 objects:")
    local objs = head(cluster_tops, 4)
    for i = 1, #objs do
        for j = i + 1, #objs do
            local a, b = objs[i].idx, objs[j].idx
            local lca = common_dominator(a, b)
            if lca >= 0 then
                local lca_cls = class_name(lca)
                local lca_ret = retained(lca)
                local lca_depth = dominator_depth(lca)
                print(string.format("  [%d]×[%d] → LCA [%d] %-32s  %s  depth=%d",
                    a, b, lca, string.sub(lca_cls, 1, 32),
                    format_bytes(lca_ret), lca_depth))
            end
        end
    end
end

-- ═══════════════════════════════════════════════════════════════════════════
section(13, "WASTE ANALYSIS — over-allocated collections (OQL lacks computed metrics)")
-- OQL can find HashMaps but cannot compute their load factor by reading
-- both the .size field (entry count) and the backing array length.
-- We approximate by comparing shallow vs retained size as a proxy.
-- ═══════════════════════════════════════════════════════════════════════════

-- Instances of HashMap with large shallow (pre-allocated capacity) but
-- small retained (few entries): high shallow/retained ratio = waste
local hmaps = instances_of("HashMap", 200)
local waste_candidates = filter(hmaps, function(h)
    return h.shallow > 0 and h.retained > 0 and h.shallow * 3 > h.retained
end)
waste_candidates = sort_by(waste_candidates, "shallow", true)

if #waste_candidates > 0 then
    print(string.format("  Found %d HashMaps where shallow > retained/3 (over-allocated):", #waste_candidates))
    print(string.format("  %-8s %-40s %10s %10s %8s",
        "IDX", "CLASS", "SHALLOW", "RETAINED", "RATIO"))
    print(string.rep("-", 80))
    for _, h in ipairs(head(waste_candidates, 8)) do
        local ratio = h.retained > 0 and (h.shallow / h.retained) or 999
        print(string.format("  [%6d] %-40s %10s %10s %6.1fx",
            h.idx, string.sub(h.class, 1, 40),
            format_bytes(h.shallow), format_bytes(h.retained), ratio))
    end
    local total_shallow_waste = sum(waste_candidates, "shallow")
    print(string.format("  Total shallow in over-allocated HashMaps: %s", format_bytes(total_shallow_waste)))
else
    print("  No obviously over-allocated HashMaps found.")
end

-- ═══════════════════════════════════════════════════════════════════════════
section(14, "NAMED VIEWS + REPORT DEEP-DIVE")
-- ═══════════════════════════════════════════════════════════════════════════

-- list_views prints them; run_view executes one
list_views()
local threads = report("threads")
if threads and type(threads) == "table" then
    local n = 0
    for _ in pairs(threads) do n = n + 1 end
    print(string.format("\n  Thread report: %d fields available", n))
end

local gc_roots = report("gc-roots")
if gc_roots and gc_roots.gc_roots then
    print(string.format("  GC roots: %s", fmt_int(gc_roots.gc_roots)))
end

local strings_rpt = report("strings")
if strings_rpt and strings_rpt.string_count then
    print(string.format("  String instances: %s", fmt_int(strings_rpt.string_count)))
    if strings_rpt.duplicate_strings then
        local dups = strings_rpt.duplicate_strings
        if type(dups) == "table" and dups.count then
            print(string.format("  Duplicate strings: %s", fmt_int(dups.count)))
        end
    end
end

-- ═══════════════════════════════════════════════════════════════════════════
section(15, "JSON EXPORT — structured findings")
-- ═══════════════════════════════════════════════════════════════════════════

local top5_suspects = {}
for i, s in ipairs(head(suspects or {}, 5)) do
    top5_suspects[i] = {
        idx           = s.idx,
        class         = s.class,
        retained      = s.retained,
        heap_pct      = pct(s.retained, heap_bytes),
        ratio         = s.ratio,
        chain_length  = 0,  -- would be enriched in a real run
    }
end

local export = {
    dump          = info.dump,
    heap_bytes    = heap_bytes,
    n_objects     = n_objects,
    n_classes     = info.classes,
    top_packages  = map(head(pkg_rows or {}, 5), function(r)
        return {package = r.package, retained = r.retained,
                heap_pct = pct(r.retained, heap_bytes)}
    end),
    outlier_count = #outliers,
    chain_count   = #chains,
    suspects      = top5_suspects,
}

local json_str  = json_encode(export)  -- json_encode via host
local compact   = json(export)         -- json() prelude alias
local roundtrip = json_decode(compact)
print(string.format("  Export: %s bytes JSON, round-trip heap=%s",
    fmt_int(#json_str), format_bytes(roundtrip.heap_bytes or 0)))
pp(export)  -- pretty-print as JSON

print(string.format("\n%s\n  ANALYSIS COMPLETE — %s objects, %s classes, heap %s\n%s",
    SEP, fmt_int(n_objects), fmt_int(info.classes), format_bytes(heap_bytes), SEP))

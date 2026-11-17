-- analysis_interactive.lua — Beyond-OQL recipes for the REPL
-- ═══════════════════════════════════════════════════════════════════════════
-- 20 self-contained recipes. Each can be pasted into the Lua REPL or run
-- as a standalone script. Every recipe does something OQL cannot do.
--
-- Usage: hprof-analyzer heap script lua dump.hprof --file analysis_interactive.lua
-- ═══════════════════════════════════════════════════════════════════════════

local info       = dump_info()
local heap_bytes = info.heap_bytes

-- ═══════════════════════════════════════════════════════════════════════════
print("── RECIPE 1: QUICK TRIAGE WITH PERCENTAGES ──")
-- OQL cannot compute % of total heap per class.
-- ═══════════════════════════════════════════════════════════════════════════

local top5 = class_stats(5)
print(string.format("  Heap: %s across %s objects", format_bytes(heap_bytes), fmt_int(info.objects)))
for i, s in ipairs(top5) do
    print(string.format("  %d. %-44s %s  (%s)",
        i, s.class, format_bytes(s.total_retained), pct(s.total_retained, heap_bytes)))
end

-- ═══════════════════════════════════════════════════════════════════════════
print("\n── RECIPE 2: MULTI-LEVEL DOMINATOR CASCADE ──")
-- OQL selects objects flatly. Scripts recursively drill through the
-- dominator tree: suspect → children → grandchildren.
-- ═══════════════════════════════════════════════════════════════════════════

local suspects = accumulation_points(heap_bytes / 200, 8.0, 3)
for _, s in ipairs(suspects) do
    print(string.format("  SUSPECT [%d] %s  %s  ratio=%.1fx",
        s.idx, s.class, format_bytes(s.retained), s.ratio))

    local children1 = big_drops(s.idx, 3)
    for _, c in ipairs(children1) do
        print(string.format("    L1 [%d] %s  %s",
            c.idx, c.class, format_bytes(c.retained)))

        local children2 = big_drops(c.idx, 2)
        for _, gc in ipairs(children2) do
            print(string.format("      L2 [%d] %s  %s",
                gc.idx, gc.class, format_bytes(gc.retained)))
        end
    end
end

-- ═══════════════════════════════════════════════════════════════════════════
print("\n── RECIPE 3: OUTLIER INSTANCES (statistical, impossible in OQL) ──")
-- OQL cannot compute per-class averages and flag deviating instances.
-- ═══════════════════════════════════════════════════════════════════════════

local stats = class_stats(50)
-- filter: classes where the outlier is > 20x the average
local skewed = filter(stats, function(s)
    return s.instances > 10
       and s.avg_retained > 0
       and s.max_retained > s.avg_retained * 20
end)

for _, s in ipairs(head(skewed, 5)) do
    local ratio = s.max_retained / s.avg_retained
    print(string.format("  %s: %s instances, avg=%s, outlier=%s (%.0fx avg)",
        s.class, fmt_int(s.instances),
        format_bytes(s.avg_retained), format_bytes(s.max_retained), ratio))
    -- inspect the actual outlier object
    local obj = inspect(s.max_retained_idx)
    print(string.format("    outlier [%d] depth=%d  idom=[%d] %s",
        s.max_retained_idx,
        dominator_depth(s.max_retained_idx),
        obj.idom, obj.idom >= 0 and class_name(obj.idom) or "<root>"))
end

-- ═══════════════════════════════════════════════════════════════════════════
print("\n── RECIPE 4: CROSS-CLASS RETENTION COMPARISON ──")
-- Compare who retains byte[], String, and HashMap — in one pass.
-- OQL must run separate queries per class and cannot compare results.
-- ═══════════════════════════════════════════════════════════════════════════

local votes = {}
for _, tgt in ipairs({"byte[]", "java.lang.String", "java.util.HashMap"}) do
    local tot = class_total(tgt)
    print(string.format("  %s: %s instances  %s  (%s)",
        tgt, fmt_int(tot.instances), format_bytes(tot.retained),
        pct(tot.retained, heap_bytes)))
    for _, r in ipairs(who_retains(tgt, 4)) do
        votes[r.class] = (votes[r.class] or 0) + 1
        print(string.format("    <- %-40s %s instances", r.class, fmt_int(r.instances)))
    end
end
-- classes retaining all three: universal containers
for cls, v in pairs(votes) do
    if v >= 3 then print("  *** UNIVERSAL CONTAINER: " .. cls) end
end

-- ═══════════════════════════════════════════════════════════════════════════
print("\n── RECIPE 5: ONE-CALL INVESTIGATION ──")
-- Uses the prelude investigate() which finds accumulation points,
-- walks their paths, and shows drops — all in one call.
-- ═══════════════════════════════════════════════════════════════════════════

investigate(heap_bytes / 100, 5)

-- ═══════════════════════════════════════════════════════════════════════════
print("\n── RECIPE 6: PACKAGE-LEVEL MEMORY ROLLUP ──")
-- OQL cannot GROUP BY derived fields like the package prefix of a class name.
-- ═══════════════════════════════════════════════════════════════════════════

local hist = histogram(300)
local pkg = {}
for _, h in ipairs(hist) do
    local dot = (h.class):find("%.")
    local top = dot and (h.class):sub(1, dot - 1) or "(none)"
    pkg[top] = (pkg[top] or 0) + h.retained_bytes
end
local pkg_rows = {}
for p, r in pairs(pkg) do pkg_rows[#pkg_rows + 1] = {package = p, retained = r} end
pkg_rows = sort_by(pkg_rows, "retained", true)
for _, row in ipairs(head(pkg_rows, 10)) do
    print(string.format("  %-20s  %10s  %s",
        row.package, format_bytes(row.retained), pct(row.retained, heap_bytes)))
end

-- ═══════════════════════════════════════════════════════════════════════════
print("\n── RECIPE 7: DOMINATOR DEPTH HISTOGRAM ──")
-- OQL has no concept of dominator-tree depth. Scripts compute a
-- distribution across the top objects to reveal tree structure.
-- ═══════════════════════════════════════════════════════════════════════════

local tops100 = top_objects(200)
local d_hist = {}
for _, obj in ipairs(tops100) do
    local d = math.min(dominator_depth(obj.idx), 12)
    d_hist[d] = (d_hist[d] or 0) + 1
end
local d_rows = {}
for d, cnt in pairs(d_hist) do d_rows[#d_rows + 1] = {depth = d, count = cnt} end
d_rows = sort_by(d_rows, "depth", false)
for _, row in ipairs(d_rows) do
    local bar = string.rep("|", math.floor(row.count / 3))
    print(string.format("  depth %2d  %4d objects  %s", row.depth, row.count, bar))
end

-- ═══════════════════════════════════════════════════════════════════════════
print("\n── RECIPE 8: RETAINED SET DIFFING ──")
-- Compare the retained set composition of two suspects.
-- OQL cannot compare retained sets across objects.
-- ═══════════════════════════════════════════════════════════════════════════

local s2 = accumulation_points(heap_bytes / 50, 5.0, 2)
if #s2 >= 2 then
    local function rs_map(idx)
        local result = {}
        for _, r in ipairs(retained_set_summary(idx)) do
            result[r.class] = r.retained
        end
        return result
    end

    local rs_a = rs_map(s2[1].idx)
    local rs_b = rs_map(s2[2].idx)

    print(string.format("  Comparing [%d] %s vs [%d] %s",
        s2[1].idx, s2[1].class, s2[2].idx, s2[2].class))

    -- Classes in A but not B (unique to A)
    local only_a = filter(
        map(s2[1] and {} or {},  function(x) return x end),  -- placeholder
        function(x) return true end)

    for cls, ret_a in pairs(rs_a) do
        local ret_b = rs_b[cls] or 0
        if ret_a > 0 and ret_b == 0 then
            print(string.format("  ONLY-A: %-40s %s", cls, format_bytes(ret_a)))
        elseif ret_a > 0 and ret_b > 0 then
            local diff = ret_a - ret_b
            if math.abs(diff) > 1048576 then
                print(string.format("  DIFF  : %-40s A=%s  B=%s  delta=%s",
                    cls, format_bytes(ret_a), format_bytes(ret_b), format_bytes(diff)))
            end
        end
    end
else
    print("  Need at least 2 suspects for diffing.")
end

-- ═══════════════════════════════════════════════════════════════════════════
print("\n── RECIPE 9: OQL + ENRICHMENT PIPELINE ──")
-- Run OQL for selection, then use scripting for enrichment.
-- The enrichment (dominator depth, parent class, retained%) is
-- completely outside OQL's capabilities.
-- ═══════════════════════════════════════════════════════════════════════════

local rows = oql([[
    SELECT @objectId AS idx, @retainedHeapSize AS ret
    FROM   java.util.ArrayList
    ORDER  BY ret DESC LIMIT 6
]])

print(string.format("  %-8s %-36s %10s %6s %5s",
    "IDX", "IDOM_CLASS", "RETAINED", "HEAP%", "DEPTH"))
print(string.rep("-", 72))
for _, row in ipairs(rows) do
    local par   = idom(row.idx)
    local pcls  = par >= 0 and class_name(par) or "<root>"
    local depth = dominator_depth(row.idx)
    print(string.format("  [%6d] %-36s %10s %6s %5d",
        row.idx, string.sub(pcls, 1, 36),
        format_bytes(row.ret), pct(row.ret, heap_bytes), depth))
end

-- ═══════════════════════════════════════════════════════════════════════════
print("\n── RECIPE 10: COLLECTION WASTE VIA SHALLOW/RETAINED RATIO ──")
-- OQL cannot compute the ratio of shallow to retained size. A high
-- shallow/retained ratio means an object uses more space than it's worth.
-- ═══════════════════════════════════════════════════════════════════════════

local lists = instances_of("ArrayList", 300)
local bloated = filter(lists, function(o)
    return o.shallow > 512 and o.retained > 0 and o.shallow > o.retained * 2
end)
bloated = sort_by(bloated, "shallow", true)

print(string.format("  %d/%d ArrayLists have shallow > 2x retained (pre-allocated, mostly empty):",
    #bloated, #lists))
for _, o in ipairs(head(bloated, 6)) do
    local ratio = o.shallow / math.max(o.retained, 1)
    print(string.format("  [%d] shallow=%s  retained=%s  ratio=%.1fx",
        o.idx, format_bytes(o.shallow), format_bytes(o.retained), ratio))
end

-- ═══════════════════════════════════════════════════════════════════════════
print("\n── RECIPE 11: CHAIN DETECTION + IMPACT SCORING ──")
-- OQL cannot detect dominator-tree chains or score them by impact.
-- ═══════════════════════════════════════════════════════════════════════════

local chains = detect_chains(4, 15)
-- Enrich: compute retained per node in chain as proxy for "compactness"
local scored = map(chains, function(c)
    local per_node = c.total_retained / math.max(c.length, 1)
    return {
        class       = c.class,
        length      = c.length,
        total       = c.total_retained,
        per_node    = per_node,
        heap_pct    = pct(c.total_retained, heap_bytes),
    }
end)
scored = sort_by(scored, "total", true)

if #scored > 0 then
    print(string.format("  %-44s %6s %12s %10s", "CLASS", "LEN", "TOTAL", "HEAP%"))
    for _, c in ipairs(head(scored, 8)) do
        print(string.format("  %-44s %6d %12s %10s",
            string.sub(c.class, 1, 44), c.length,
            format_bytes(c.total), c.heap_pct))
    end
end

-- ═══════════════════════════════════════════════════════════════════════════
print("\n── RECIPE 12: OWNERSHIP CHAIN RECONSTRUCTION ──")
-- Walk from an object upward through its entire idom chain, annotating
-- each level. OQL has no idom traversal.
-- ═══════════════════════════════════════════════════════════════════════════

local seed_objs = top_objects(1)
if #seed_objs > 0 then
    local idx = seed_objs[1].idx
    print(string.format("  Ownership chain from [%d] %s:", idx, class_name(idx)))
    local path = dominator_path(idx)
    for level, p in ipairs(path) do
        local indent = string.rep("  ", level)
        local ratio  = p.retained > 0 and pct(p.retained, heap_bytes) or "0%"
        print(string.format("  %s[%d] %-40s  %s  (%s)",
            indent, p.idx, p.class, format_bytes(p.retained), ratio))
    end
end

-- ═══════════════════════════════════════════════════════════════════════════
print("\n── RECIPE 13: REFERENCE GRAPH — WHO CREATES LARGE ARRAYS? ──")
-- OQL cannot traverse reference edges or compute inbound ref statistics.
-- ═══════════════════════════════════════════════════════════════════════════

if is_graph_loaded() then
    local inbound = incoming_by_class("byte[]", 10)
    print("  Classes that hold references into byte[] arrays (top by ref count):")
    for _, r in ipairs(inbound) do
        print(string.format("  %-44s %6s refs  %s retained",
            r.class, fmt_int(r.ref_count), format_bytes(r.total_retained)))
    end

    -- Walk outbound refs of the biggest object to understand its graph
    local big = top_objects(1)[1]
    local out = outbound_refs(big.idx)
    print(string.format("\n  [%d] %s has %d outbound refs. Enriched top 5:",
        big.idx, big.class, #out))
    local enriched_out = map(out, function(oidx)
        return {idx = oidx, cls = class_name(oidx), ret = retained(oidx)}
    end)
    enriched_out = sort_by(enriched_out, "ret", true)
    for _, o in ipairs(head(enriched_out, 5)) do
        print(string.format("  -> [%d] %-40s %s", o.idx, o.cls, format_bytes(o.ret)))
    end
else
    print("  Skip (requires --with-graph)")
end

-- ═══════════════════════════════════════════════════════════════════════════
print("\n── RECIPE 14: FULL RETAINED SET EXPLORATION ──")
-- retained_set() gives every object in the dominator subtree with
-- optional class filtering — OQL cannot enumerate a subtree.
-- ═══════════════════════════════════════════════════════════════════════════

local big2 = top_objects(1)[1]
-- get full retained set with class filter to arrays only
local arr_set = retained_set(big2.idx, "[]", 30)
if arr_set then
    print(string.format("  [%d] retains %s objects total, showing array types:",
        big2.idx, fmt_int(arr_set.total_objects_in_subtree)))
    print(string.format("  total shallow in subtree: %s",
        format_bytes(arr_set.total_shallow_in_subtree)))
    for _, o in ipairs(head(arr_set.objects or {}, 8)) do
        print(string.format("  depth=%d [%d] %-40s %s",
            o.depth, o.index, o.class, format_bytes(o.shallow_bytes)))
    end
end

-- ═══════════════════════════════════════════════════════════════════════════
print("\n── RECIPE 15: CLASS HIERARCHY RETENTION ANALYSIS ──")
-- Compare retention profiles of all classes matching a pattern.
-- OQL returns all instances of one class; scripts aggregate across
-- all matching class names and produce cross-class statistics.
-- ═══════════════════════════════════════════════════════════════════════════

local map_classes = filter(class_stats(200), function(s)
    return s.class:find("Map") ~= nil or s.class:find("Set") ~= nil
end)
map_classes = sort_by(map_classes, "total_retained", true)

print(string.format("  %-44s %6s %10s %10s", "CLASS", "COUNT", "TOTAL", "AVG"))
for _, s in ipairs(head(map_classes, 10)) do
    print(string.format("  %-44s %6s %10s %10s",
        string.sub(s.class, 1, 44), fmt_int(s.instances),
        format_bytes(s.total_retained), format_bytes(s.avg_retained)))
end

-- ═══════════════════════════════════════════════════════════════════════════
print("\n── RECIPE 16: COMMON DOMINATOR CLUSTERING ──")
-- Compute pairwise LCAs for top objects to cluster them by shared
-- retention root. OQL cannot compute LCAs or build clusters.
-- ═══════════════════════════════════════════════════════════════════════════

local ctops = head(top_objects(8), 8)
local lca_counts = {}
for i = 1, #ctops do
    for j = i + 1, #ctops do
        local lca = common_dominator(ctops[i].idx, ctops[j].idx)
        if lca >= 0 then
            local lcls = class_name(lca)
            lca_counts[lcls] = (lca_counts[lcls] or 0) + 1
        end
    end
end
local lca_rows = {}
for cls, cnt in pairs(lca_counts) do
    lca_rows[#lca_rows + 1] = {class = cls, count = cnt}
end
lca_rows = sort_by(lca_rows, "count", true)
print("  Most common LCA classes among top 8 objects:")
for _, row in ipairs(head(lca_rows, 5)) do
    print(string.format("  %-44s appears as LCA %d times", row.class, row.count))
end

-- ═══════════════════════════════════════════════════════════════════════════
print("\n── RECIPE 17: DOMINATOR SUBTREE WALK + CLASS AGGREGATION ──")
-- Walk the dominator subtree of the biggest object and aggregate
-- retained bytes by class within it. OQL cannot walk subtrees.
-- ═══════════════════════════════════════════════════════════════════════════

local big3     = top_objects(1)[1]
local subtree  = walk_subtree(big3.idx, 4, 200)
-- use prelude walk_tree for visual output at depth 2
walk_tree(big3.idx, 2, 8)

-- Aggregate retained by class within the subtree
local sub_by_cls = {}
for _, node in ipairs(subtree) do
    sub_by_cls[node.class] = (sub_by_cls[node.class] or 0) + node.retained
end
local sub_rows = {}
for cls, ret in pairs(sub_by_cls) do
    sub_rows[#sub_rows + 1] = {class = cls, retained = ret}
end
sub_rows = sort_by(sub_rows, "retained", true)
print(string.format("\n  Classes in subtree of [%d] (top 5):", big3.idx))
for _, row in ipairs(head(sub_rows, 5)) do
    print(string.format("  %-40s %s", row.class, format_bytes(row.retained)))
end

-- ═══════════════════════════════════════════════════════════════════════════
print("\n── RECIPE 18: THREAD-TO-OBJECT ATTRIBUTION ──")
-- Combine report sections (threads) with dominator analysis to attribute
-- retention to specific threads. OQL cannot cross report and heap data.
-- ═══════════════════════════════════════════════════════════════════════════

local threads_rpt = report("threads")
if threads_rpt and type(threads_rpt) == "table" then
    print("  Thread report available — finding Thread objects in heap...")
    local thread_objs = instances_of("java.lang.Thread", 20)
    print(string.format("  Found %d Thread instances in heap", #thread_objs))
    for _, t in ipairs(head(thread_objs, 4)) do
        local ret_summary = head(retained_set_summary(t.idx), 3)
        print(string.format("  Thread [%d]  retained=%s  top classes:",
            t.idx, format_bytes(t.retained)))
        for _, rs in ipairs(ret_summary) do
            print(string.format("    %-36s %s instances  %s",
                rs.class, fmt_int(rs.count), format_bytes(rs.retained)))
        end
    end
else
    print("  Thread report not available in this dump.")
end

-- ═══════════════════════════════════════════════════════════════════════════
print("\n── RECIPE 19: NAMED VIEWS + OQL RAW METADATA ──")
-- ═══════════════════════════════════════════════════════════════════════════

list_views()
local raw = oql_raw([[
    SELECT classof(x) AS cls, count() AS cnt
    FROM   java.lang.Object x
    LIMIT  5
]])
if raw then
    print(string.format("  OQL raw: %s total rows, truncated=%s, columns: %s",
        fmt_int(raw.row_count or 0),
        tostring(raw.truncated or false),
        table.concat(raw.columns or {}, ", ")))
end

-- ═══════════════════════════════════════════════════════════════════════════
print("\n── RECIPE 20: FULL AUTOMATED TRIAGE FUNCTION ──")
-- Assembles findings from multiple analyses into a single triage report.
-- No single OQL query can cover all these dimensions simultaneously.
-- ═══════════════════════════════════════════════════════════════════════════

local function full_triage()
    local findings = {}

    -- 1. biggest class by retained
    local top_class = class_stats(1)[1]
    if top_class then
        findings[#findings + 1] = string.format(
            "Top class: %s  %s  (%s of heap)",
            top_class.class, format_bytes(top_class.total_retained),
            pct(top_class.total_retained, heap_bytes))
    end

    -- 2. biggest accumulation point
    local pts = accumulation_points(heap_bytes / 50, 5.0, 1)
    if #pts > 0 then
        findings[#findings + 1] = string.format(
            "Biggest suspect: [%d] %s  %s  ratio=%.1fx",
            pts[1].idx, pts[1].class, format_bytes(pts[1].retained), pts[1].ratio)
    end

    -- 3. chain summary
    local ch = detect_chains(5, 3)
    if #ch > 0 then
        findings[#findings + 1] = string.format(
            "Longest chain: %d x %s  (%s)",
            ch[1].length, ch[1].class, format_bytes(ch[1].total_retained))
    end

    -- 4. string waste
    local str_tot = class_total("java.lang.String")
    findings[#findings + 1] = string.format(
        "Strings: %s instances  %s  (%s)",
        fmt_int(str_tot.instances), format_bytes(str_tot.retained),
        pct(str_tot.retained, heap_bytes))

    -- 5. byte[] total
    local bt_tot = class_total("byte[]")
    findings[#findings + 1] = string.format(
        "byte[]: %s instances  %s",
        fmt_int(bt_tot.instances), format_bytes(bt_tot.retained))

    print("\n  ╔══ TRIAGE SUMMARY ══╗")
    for i, f in ipairs(findings) do
        print(string.format("  %d. %s", i, f))
    end
    print("  ╚════════════════════╝\n")

    return pp({heap = heap_bytes, findings = findings})
end

full_triage()

print("\n═══ ALL 20 RECIPES COMPLETE ═══")

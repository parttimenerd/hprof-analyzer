#!/usr/bin/env python3
"""Arithmetic consistency checks for visualization-demos.html mock data."""

print("=" * 60)
print("DEMO 1 — Sunburst domTree data consistency")
print("=" * 60)

root_retained = 158_000_000
root_shallow = 48
child_node_arr = 142_000_000  # HashMap$Node[]
child_integer = 12_000_000    # Integer
children_sum = child_node_arr + child_integer
diff = root_retained - children_sum
print(f"Root HashMap retained:       {root_retained:>15,}")
print(f"Children sum (Node[]+Int):   {children_sum:>15,}")
print(f"Diff (should be ~shallow=48):{diff:>15,}  <-- OFF BY {diff:,}")

print()
# Node[] retained=142M, shallow=4000048, children sum = 68+52+18 = 138M
node_arr_retained = 142_000_000
node_arr_shallow = 4_000_048
node_children_sum = 68_000_000 + 52_000_000 + 18_000_000
print(f"Node[] retained:   {node_arr_retained:>15,}")
print(f"Node[] children sum: {node_children_sum:>13,}")
print(f"Diff (should be shallow={node_arr_shallow}): {node_arr_retained - node_children_sum:,}  OK={abs(node_arr_retained - node_children_sum - node_arr_shallow) < 1000}")

print()
# Node[0] retained=68M, shallow=32, children: CacheEntry=45M + String=22M = 67M
node0_retained = 68_000_000
node0_shallow = 32
node0_children_sum = 45_000_000 + 22_000_000
print(f"Node[0] retained:    {node0_retained:>15,}")
print(f"Node[0] children sum:{node0_children_sum:>15,}")
print(f"Diff (should be shallow={node0_shallow}): {node0_retained - node0_children_sum:,}  <-- OFF BY {node0_retained - node0_children_sum - node0_shallow:,}")

print()
# Node[1] retained=52M, shallow=32, children: SessionData=48M
node1_retained = 52_000_000
node1_shallow = 32
node1_children_sum = 48_000_000
print(f"Node[1] retained:    {node1_retained:>15,}")
print(f"Node[1] children sum:{node1_children_sum:>15,}")
print(f"Diff:                {node1_retained - node1_children_sum:>15,}  (large, unexplained by shallow=32)")

print()
# SessionData retained=48M, children: ArrayList=35M + byte[]=12.5M = 47.5M
sess_retained = 48_000_000
sess_shallow = 64
sess_children_sum = 35_000_000 + 12_500_000
print(f"SessionData retained:{sess_retained:>15,}")
print(f"SessionData children:{sess_children_sum:>15,}")
print(f"Diff (should be shallow={sess_shallow}): {sess_retained - sess_children_sum:,}")

print()
# CacheEntry retained=45M, children: byte[]=42M + String=2.8M = 44.8M
cache_retained = 45_000_000
cache_shallow = 24
cache_children_sum = 42_000_000 + 2_800_000
print(f"CacheEntry retained: {cache_retained:>15,}")
print(f"CacheEntry children: {cache_children_sum:>15,}")
print(f"Diff (should be ~shallow={cache_shallow}): {cache_retained - cache_children_sum:,}  OK")

print()
# String retained=22M (in domTree), but Sankey sends 24.5M from String->byte[]
print("String retained in domTree:         22,000,000")
print("String->byte[] flow in Sankey:      24,500,000  <-- INCONSISTENT")
print("Excess:                             2,500,000")

print()
# Heatmap: HashMap$Node total dominated bytes vs Node retained
hm_node_to_ce = 45_000_000
hm_node_to_str = 22_000_000
hm_node_to_sess = 48_000_000
hm_node_to_mb = 17_500_000
hm_node_dominated_total = hm_node_to_ce + hm_node_to_str + hm_node_to_sess + hm_node_to_mb
print(f"Heatmap Node total dominated retained: {hm_node_dominated_total:,}")
print(f"But Node retained in domTree: 68,000,000 (first Node)")
print(f"This is the aggregate over ALL Node instances, so heatmap pairs are cross-instance")

print()
print("=" * 60)
print("DEMO 7 — Category Budget: uses 'retained' not 'shallow'")
print("=" * 60)
cat_app = 110_000_000
cat_jdk = 85_000_000
cat_fw = 45_000_000
cat_total = cat_app + cat_jdk + cat_fw
total_heap_sunburst = 240_000_000
print(f"Category total (retained sum): {cat_total:,}")
print(f"totalHeap in demo 1:           {total_heap_sunburst:,}  MATCH")
print("WARNING: Spec rule 4 says use SHALLOW bytes for additive composition.")
print("Demo 7 uses 'retained' — retained bytes are nested/overlapping.")

print()
print("=" * 60)
print("DEMO 10 — Waste: label uses var(--amber) twice")
print("=" * 60)
print("wasteColors[1] = var(--amber)  for 'Empty collections'")
print("wasteColors[2] = var(--amber)  for 'Over-capacity arrays'")
print("Legend: both categories show same amber color — confusing")

print()
print("=" * 60)
print("DEMO 11 — Boxed Number Waste (J1): pctOfHeap vs total heap")
print("=" * 60)
# Integer: 4544000 bytes, pctOfHeap=3.2% => implied heap = 4544000/0.032 = 142M
# Long: 2184000/0.0154 = 141.8M
# Double: 1080000/0.0076 = 142.1M
# Boolean: 192000/0.0014 = 137.1M -- off
# Short: 128000/0.0009 = 142.2M
for name, shallow, pct in [
    ("Integer", 4_544_000, 3.2),
    ("Long", 2_184_000, 1.54),
    ("Double", 1_080_000, 0.76),
    ("Boolean", 192_000, 0.14),
    ("Short", 128_000, 0.09),
]:
    implied_heap = shallow / (pct / 100)
    print(f"  {name}: shallow={shallow:,}, pct={pct}% => implied heap={implied_heap:,.0f}")

print()
print("=" * 60)
print("DEMO 11 — KPI 'of total heap (largest)' shows data[0].pctOfHeap = 3.2%")
print("  but demo 1 totalHeap=240M; Integer shallow=4.544M/240M=1.89%, not 3.2%")
print("=" * 60)
integer_shallow = 4_544_000
total_heap = 240_000_000
actual_pct = integer_shallow / total_heap * 100
print(f"  Integer shallow / totalHeap = {actual_pct:.2f}%  (mock says 3.2% — inconsistent)")

print()
print("=" * 60)
print("DEMO 14 — E3 thread bar: 'GC Thread#0' with 200K retained")
print("=" * 60)
print("GC threads are not Java threads; they should not appear in ThreadInfo.retained")
print("Including GC Thread#0 is misleading / inaccurate")

print()
print("=" * 60)
print("DEMO 15 — N1 Large Array Facts: regionSize hardcoded")
print("=" * 60)
print("Line 917: regionSize = 4_194_304 (4MB G1HeapRegionSize)")
print("But line 939 says 'display marker (not a GC setting)' — good")
print("HOWEVER: variable is named regionSize which contradicts the guardrail")

print()
print("=" * 60)
print("DEMO 16 — L2 Map Bucket Occupancy: lower/upper appear unused")
print("=" * 60)
print("Each bucket has lower/upper fields that are defined but never used in rendering")

print()
print("=" * 60)
print("DEMO 17 — B4: totalCap - uniqueCap slice overlap text")
print("=" * 60)
totalCap = 186_000_000
uniqueCap = 149_000_000
slices = totalCap - uniqueCap
print(f"totalCap={totalCap:,}, uniqueCap={uniqueCap:,}, diff={slices:,}")
print(f"KPI says: 'includes {slices:,} of slices' — this is ambiguous (capacity of slices or count?)")

print()
print("=" * 60)
print("DEMO 19 — N2 Finalizer: total vs sum of rows")
print("=" * 60)
total_fin = 12_800
row_counts = [6_150, 2_900, 1_700, 1_250, 800]
row_sum = sum(row_counts)
print(f"Declared total: {total_fin:,}")
print(f"Sum of rows: {row_sum:,}  MATCH: {row_sum == total_fin}")

print()
print("=" * 60)
print("DEMO 22 — K5 Group by Field: total check")
print("=" * 60)
groups = [412_000_000, 66_000_000, 51_000_000, 35_000_000, 9_000_000, 28_000_000]
g_total = sum(groups)
print(f"Sum of group shallow bytes: {g_total:,}")
print(f"tenant-042 share: {groups[0]/g_total*100:.1f}%  (expected ~68%)")
print(f"Signal fires at >=50% and >=32MB: {groups[0]/g_total >= 0.5 and groups[0] >= 32_000_000}")

print()
print("=" * 60)
print("DEMO 6 — Sparklines: single scale for all classes")
print("=" * 60)
# All rows use the same max/min per-row scale, not a shared scale
print("Each sparkline uses per-row min/max scaling, not a shared scale.")
print("The 'What to look for' text says 'All rows share one scale' — this is FALSE.")

print()
print("=" * 60)
print("DEMO 13 — K3 Scatter: hardcoded non-CSS colors")
print("=" * 60)
print("Lines 819, 846: uses #e74c3c, #e67e22, #f1c40f, #3498db — hardcoded hex")
print("These are flat-ui colors that won't adapt to dark mode")
print("Also diagonal uses #ddd and #aaa — hardcoded")

print()
print("=" * 60)
print("DEMO 16 — L2: hardcoded hex colors in buckets array")
print("=" * 60)
print("Lines 987-992: bucket colors #2ecc71, #27ae60, #f1c40f, #e67e22, #e74c3c, #c0392b")
print("None of these respect the CSS variable theme system")

print()
print("=" * 60)
print("STRUCTURAL: script ordering")
print("=" * 60)
print("Line 164-200: shared utilities (window.PALETTE etc.) in plain <script>")
print("Line 202+: module script uses 'await import()' at top level")
print("Demos 11+ use non-module <script> tags that call fmtBytes/PALETTE etc.")
print("The module script (line 202) runs AFTER the shared utilities, but")
print("the non-module scripts at lines 615, 671, etc. are ALSO non-module and")
print("run after utilities - that part is fine.")
print("BUT: demos 11-22 are separate <script> tags AFTER the module script.")
print("Module scripts are deferred; the non-module scripts may execute before")
print("the module finishes, so the DOM elements for demos 1-10 won't be rendered")
print("until the module resolves its CDN imports.")

print()
print("=" * 60)
print("STRUCTURAL: missing </html> tag")
print("=" * 60)
print("File ends at line 1335 with </body> but no </html>")

print()
print("=" * 60)
print("STRUCTURAL: tag inconsistency demo-card vs demo classes")
print("=" * 60)
print("Demos 1-10 use class='demo-card', demos 11-22 use class='demo'")
print("Both are defined in CSS (line 24) but visual styling may differ subtly")

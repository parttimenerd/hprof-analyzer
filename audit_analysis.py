"""
Audit analysis for retained.rs and sweep.rs
"""

print("=" * 60)
print("RETAINED.RS ANALYSIS")
print("=" * 60)

# --- RET-01: child_off degree counter overflow (line 26) ---
print("\n[RET-01] child_off u32 degree counter (line 26)")
print("  child_off: Vec<u32>; incremented once per child")
print("  n can be up to ~4.29B (u32::MAX-1 objects)")
print("  If one node has 500M children -> counter < u32::MAX, still OK")
print("  BUT: total_children = child_off[n+1] as usize")
print("  child_off[n+1] is a u32 prefix-sum; with 500M objects it easily")
print("  exceeds u32::MAX (4,294,967,295) -> wraps silently in release,")
print("  panics in debug. e.g. 5B edges total -> child_off prefix-sum wraps.")
n_example = 4_294_967_296  # 4B edges
u32_max = 4_294_967_295
print(f"  Example: if total edges = {n_example:,}")
print(f"  child_off[n+1] as u32 = {n_example % (u32_max+1):,} (WRONG wraps to 0)")

# --- RET-02: sp_new truncation at deep trees (line 156) ---
print("\n[RET-02] sp_new = (stk.len() + 1) as u32 (line 156)")
print("  If dominator-tree depth >= 2^32, sp_new wraps to 0.")
print("  sp_new = 0 -> class_to_last_depth[cls] = 0 (sentinel 'not on stack')")
print("  -> hasSameClassAncestor MISSED for class members below depth 4B")
print("  Realistic for pathological heaps (linked lists of 4B objects).")
print("  In release mode: silent wrong output; in debug mode: no panic (just u32 truncation)")

# --- RET-03: vroot initial child_idx seed (line 123) ---
print("\n[RET-03] vroot push: child_idx: child_off[n] (line 123)")
print("  child_off has length n+2.")
print("  child_off[n] is the START of vroot's children (index n).")
print("  child_off[n+1] is the END of vroot's children.")
print("  The loop check at line 137 uses child_off[v+1] as end_child.")
print("  v = vroot = n, so end_child = child_off[n+1]. CORRECT.")
print("  BUT: child_off[n] seeds child_idx for vroot frame (line 123).")
print("  After CSR restore (lines 46-49), child_off[n] = START of vroot children.")
print("  So the seed IS correct for vroot. No bug here.")

# --- RET-04: depth_counts resize fence-post (lines 162-165) ---
print("\n[RET-04] depth_counts resize + index (lines 162-165)")
print("  b2_depth = sp_new - 1 (always >= 1 since sp_new >= 2)")
print("  if b2_depth > depth_counts.len(): resize(b2_depth, 0)")
print("  After resize, len = b2_depth, valid indices 0..b2_depth-1")
print("  depth_counts[b2_depth - 1] += 1  -> last element, valid.")
print("  HOWEVER: resize uses STRICT > so when b2_depth == depth_counts.len(),")
print("  depth_counts already has b2_depth elements (0..b2_depth-1 valid).")
print("  When b2_depth > len, resize extends to exactly b2_depth. OK.")
print("  Conclusion: no off-by-one here.")

# But let's double-check the sp_new minimum
print("\n  Checking sp_new minimum:")
print("  vroot frame pushed first (stk.len()=0 before push)")
print("  When processing vroot's first child: stk.len()=1 (vroot frame on stack)")
print("  sp_new = (1 + 1) as u32 = 2; b2_depth = 1")
print("  depth_counts.resize(1, 0) -> len=1; depth_counts[0] += 1 -> OK")

# --- RET-05: retained rollup on vroot direct children ---
print("\n[RET-05] retained rollup vroot guard (lines 203-207)")
print("  if top > 0 { let parent = stk[top-1].node;")
print("    if parent != vroot { retained[parent as usize] += retained[v as usize]; }}")
print("  Objects directly under vroot have parent=vroot.")
print("  Their retained size does NOT roll up.")
print("  This is INTENTIONAL: vroot has no retained[] slot (retained has len=n).")
print("  These objects' own retained[] IS correct (their subtrees DID roll into them).")
print("  Callers only read retained[0..n]. Correct.")

print("\n" + "=" * 60)
print("SWEEP.RS ANALYSIS")
print("=" * 60)

# --- SWP-01: truncate output can exceed max bytes (lines 241-253) ---
print("\n[SWP-01] truncate byte-length contract (lines 241-253)")
print("  s.len() is BYTE length (not char count).")
print("  budget = max.saturating_sub(3) -- byte budget for prefix.")
print("  end = last char_index i where i (byte offset) <= budget.")
print("  output = s[..end] + '...' has byte length end + 3.")
print("  Worst case: end = budget = max-3, output = (max-3)+3 = max bytes. OK for ASCII.")
print("  For max <= 2: budget = 0, end could be 0, output = '...' = 3 bytes > max!")
for max_val in [0, 1, 2, 3, 4]:
    budget = max(0, max_val - 3)
    # take_while i <= budget: for ASCII last i = budget if string long enough
    end = budget  # worst case
    out_len = end + 3
    ok = out_len <= max_val
    print(f"  max={max_val}: budget={budget}, end={end}, output_len={out_len}, ok={ok}")
print("  In practice max=34 always (hardcoded call site), so this is Low severity.")

# --- SWP-02: n_fail summary vs fields divergence (lines 123-144) ---
print("\n[SWP-02] n_fail from summary vs fields walk divergence (lines 123-144)")
print("  DumpTally.n_fail = dj.summary.fail  (line 140) -- trusted from JSON summary field")
print("  DumpTally.fails = walk of dj.fields for tier=FAIL  (line 132)")
print("  These can diverge if the JSON producer miscounts summary vs actual field tiers.")
print("  gate_pass() uses total_fail() which sums n_fail (from summary).")
print("  render() FAIL list uses d.fails (from fields walk).")
print("  An adversarial or buggy producer could pass gate (summary.fail=0) while")
print("  the fields list has FAIL tier entries -- or vice versa.")
print("  Severity: Medium (trust boundary issue / audit inconsistency).")

# --- SWP-03: is_real_comparison addition overflow ---
print("\n[SWP-03] is_real_comparison addition overflow (line 81)")
print("  n_match + n_explainable + n_fail > 0")
print("  All are usize; sum could overflow if each is near usize::MAX.")
print("  Realistic? No -- these are field counts from JSON, not heap counts.")
print("  Severity: Low (theoretical).")
print("  Fix: use saturating_add or check each individually: any != 0")

# --- SWP-04: total_fail overflow ---
print("\n[SWP-04] total_fail() sum overflow (line 94)")
print("  self.dumps.iter().map(|d| d.n_fail).sum::<usize>()")
print("  With thousands of dumps each with millions of FAIL entries -> could wrap.")
print("  In practice FAIL counts are tiny. Severity: Low (theoretical).")

# --- SWP-05: render() FAIL/EXPLAINABLE list vs summary mismatch in gate ---
print("\n[SWP-05] gate uses summary.fail but audit uses fields walk (structural)")
print("  gate_pass() -> total_fail() -> n_fail (summary.fail)")
print("  render() FAIL section -> d.fails (fields walk)")
print("  These are computed from different sources. If they differ, the rendered")
print("  report could say '(none)' in FAIL section while gate_pass() returns false,")
print("  making the gate failure reason invisible to the reader.")

print("\nDone.")

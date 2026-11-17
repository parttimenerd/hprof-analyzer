"""
Check: can next_child_pos + 1 overflow u32?
next_child_pos is child_idx, a u32.
It is only incremented when next_child_pos < end_child.
end_child = child_off[v+1], a u32.
child_off prefix sum: max value = total_children (number of edges).
Edges <= n (each object appears as child exactly once in dominator tree).
n <= u32::MAX - 1 (idom is &[u32], vroot=n so n < u32::MAX).
Therefore total_children <= n <= u32::MAX - 1.
So end_child <= u32::MAX - 1.
next_child_pos < end_child <= u32::MAX - 1 -> next_child_pos <= u32::MAX - 2.
next_child_pos + 1 <= u32::MAX - 1 -> NO overflow.
"""
u32_max = 2**32 - 1
n_max = u32_max - 1  # objects, vroot = u32::MAX
total_children_max = n_max  # each object is child of exactly one dominator
end_child_max = total_children_max  # as u32
next_child_pos_max = end_child_max - 1  # must be < end_child to enter if
increment_max = next_child_pos_max + 1
print(f"n_max = {n_max}")
print(f"total_children_max = {total_children_max}")
print(f"end_child_max (u32) = {end_child_max}")
print(f"next_child_pos_max when entering if = {next_child_pos_max}")
print(f"after increment = {increment_max}")
print(f"u32::MAX = {u32_max}")
print(f"overflow? {increment_max > u32_max}")
print()

# BUT: child_off is Vec<u32> and the PREFIX SUM elements can themselves overflow!
# child_off[i] is a running sum of degrees. total_children = child_off[n+1].
# If total_children > u32::MAX, then child_off[n+1] (stored as u32) wraps.
# Then `let total_children = child_off[n+1] as usize` gets a WRONG (too small) value.
# child_tgt is allocated with wrong (too small) size.
# child_tgt[child_off[p as usize] as usize] = u as u32 -> OOB write -> panic or UB.
print("PREFIX SUM OVERFLOW CHECK:")
print(f"If actual total_children > {u32_max}, child_off[n+1] wraps.")
print(f"total_children can equal n_max = {n_max} which is {n_max > u32_max} > u32::MAX? {n_max > u32_max}")
print(f"n_max = u32::MAX - 1 = {u32_max - 1} which is < u32::MAX = {u32_max}")
print(f"So total_children <= u32::MAX - 1 < u32::MAX: prefix sum CANNOT overflow!")
print()
print("CONCLUSION: next_child_pos + 1 and prefix-sum are both safe.")
print("The u32 degree counter (child_off[p+1] += 1) is also safe:")
print("  Each u increments exactly one child_off[p+1]; p is unique per u.")
print("  So no single counter can exceed n_max = u32::MAX-1.")
print()

# BUT: prefix-sum intermediate values!
# child_off[i+1] += child_off[i]  (line 30)
# child_off[i+1] (before add) is a degree count <= n
# child_off[i] (prefix sum so far) <= sum of all previous degrees
# The final prefix sum is child_off[n+1] = total_children <= u32::MAX - 1 (safe)
# BUT intermediate values could exceed u32::MAX if the sum first grows then shrinks?
# No: prefix sums are monotonically increasing. The final value is the maximum.
# So if the final value fits in u32, all intermediates fit too.
print("Intermediate prefix sums are monotonically increasing.")
print("Final = max. If final fits in u32, all intermediate values fit too.")
print("SAFE.")

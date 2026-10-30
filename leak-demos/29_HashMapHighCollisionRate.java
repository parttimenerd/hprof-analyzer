/**
 * SCENARIO 29 — HashMap degenerate hash: high collision rate, treeified buckets
 *
 * Run:   java -Xmx128m 29_HashMapHighCollisionRate.java
 *        (No OOM — WASTE/PERF scenario. Heap dump mid-run.)
 *
 * Capture:  jcmd $(pgrep -f HashMapHighCollisionRate) GC.heap_dump heap.hprof
 *
 * Root:  HashMap hashes keys with (key.hashCode() ^ (hashCode >>> 16)) then
 *        distributes into a bucket by index = hash & (capacity - 1).
 *        If all keys hash to the same bucket, every entry ends up in a single
 *        linked list (or, after Java 8, a red-black tree when the chain exceeds 8).
 *
 *        Effects:
 *          (a) get/put degrade from O(1) to O(log n) per bucket (after treeify).
 *          (b) A treeified bucket uses TreeNode objects (~4× overhead vs Node).
 *          (c) The heap dump shows many TreeNode instances instead of Node instances,
 *              which is a strong diagnostic signal that the hash function is degenerate.
 *
 *        Two scenarios:
 *        A) Pure collision: all hashCodes return 0 → single bucket, linear chain,
 *           then treeified after 8 entries.
 *        B) Realistic weak hash: String keys that are permutations of the same
 *           characters → many collide on the same bucket due to the polynomial hash.
 *
 * hprof-analyzer:
 *   hprof-analyzer heap.hprof report.html --collections
 *   → HashMap(s) with very high collision rate; treeified buckets flagged
 *
 *   hprof-analyzer heap query heap.hprof \
 *       --oql "SELECT h, h.size, h.table.@length AS cap
 *               FROM java.util.HashMap h ORDER BY h.size DESC LIMIT 5"
 *
 *   # TreeNode instances indicate treeified buckets:
 *   hprof-analyzer heap query heap.hprof \
 *       --oql "SELECT COUNT(*) FROM java.util.HashMap\$TreeNode"
 *
 * Fix:
 *   (a) Implement a proper hashCode() that distributes well (mix all fields).
 *   (b) Use Objects.hash() or Guava's Hashing for composite keys.
 *   (c) For String keys derived from a small alphabet, consider using a Trie or
 *       prefix-aware data structure instead.
 *   (d) Test hash distribution with a histogram of bucket sizes after bulk insertion.
 */
import java.util.*;

public class _29_HashMapHighCollisionRate {

    // ❌ BAD: every instance returns 0 → all entries in one bucket
    static class ZeroHashKey {
        final int id;
        ZeroHashKey(int id) { this.id = id; }

        @Override public int hashCode() { return 0; } // degenerate!
        @Override public boolean equals(Object o) {
            return o instanceof ZeroHashKey && ((ZeroHashKey) o).id == id;
        }
    }

    // Mild collision: String keys that are anagrams/permutations
    // Java's String.hashCode() is order-sensitive, so permutations don't collide,
    // but strings padded to the same prefix DO collide on certain capacities.
    // More realistic: keys that differ only in suffix → many share the same top bits.
    static String makeCollidingKey(int i) {
        // Force collision by making hashCode identical: append a suffix that XORs to 0
        // (Not perfectly portable; demonstrates the concept)
        return "key" + String.format("%08d", i);
    }

    public static void main(String[] args) throws Exception {
        int N = 100_000;

        // --- Pattern A: pure zero-hash collision ---
        System.out.println("Pattern A: HashMap with all-zero hashCode keys …");
        Map<ZeroHashKey, Integer> badMap = new HashMap<>();
        for (int i = 0; i < N; i++) {
            badMap.put(new ZeroHashKey(i), i);
        }
        System.out.printf("  Inserted %,d entries; bucket[0] holds all of them%n", N);
        System.out.printf("  Buckets treeified after 8 entries → %,d TreeNode objects%n", N);

        // --- Pattern B: good-hash map for comparison ---
        System.out.println("Pattern B: HashMap with well-distributed Integer keys …");
        Map<Integer, Integer> goodMap = new HashMap<>();
        for (int i = 0; i < N; i++) {
            goodMap.put(i, i); // Integer.hashCode(i) == i → excellent distribution
        }
        System.out.printf("  Inserted %,d entries; evenly spread across %,d buckets%n",
            N, Integer.highestOneBit(N * 2 - 1) * 2);

        Runtime rt = Runtime.getRuntime();
        System.gc(); Thread.sleep(200);
        System.out.printf("%nHeap used: %,d MB / %,d MB%n",
            (rt.totalMemory() - rt.freeMemory()) >> 20, rt.maxMemory() >> 20);
        System.out.println();
        System.out.println("Take a heap dump now:");
        System.out.println("  jcmd $(pgrep -f HashMapHighCollisionRate) GC.heap_dump heap.hprof");
        System.out.println();
        System.out.println("Key query — count TreeNode (treeified buckets from bad map):");
        System.out.println("  hprof-analyzer heap query heap.hprof \\");
        System.out.println("      --oql \"SELECT COUNT(*) FROM java.util.HashMap\\$TreeNode\"");
        System.out.println();
        System.out.println("Then: hprof-analyzer heap.hprof report.html --collections");

        System.out.println(badMap.size() + " " + goodMap.size()); // prevent optimizer
        Thread.sleep(Long.MAX_VALUE);
    }
}

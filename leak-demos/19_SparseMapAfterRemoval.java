/**
 * SCENARIO 19 — Sparse HashMap after bulk removal: table never shrinks
 *
 * Run:   java -Xmx128m 19_SparseMapAfterRemoval.java
 *        (No OOM — WASTE scenario. Heap dump mid-run.)
 *
 * Capture:  jcmd $(pgrep -f SparseMapAfterRemoval) GC.heap_dump heap.hprof
 *
 * Root:  HashMap grows its internal Node[] table when size > capacity * loadFactor.
 *        It NEVER shrinks the table on removals — there is no "shrink when sparse" path.
 *        A map that once held 1 000 000 entries and had all but 100 removed still
 *        occupies a table of length ~1 048 576 (the next power of two it grew to).
 *
 *        Fill ratio: 100 / 1 048 576 ≈ 0.01% — essentially empty, but full cost kept.
 *
 *        Real-world trigger: a cache that warms up at startup with many entries, then
 *        entries expire or are invalidated one by one. The backing table never reclaims
 *        its memory regardless of how empty the map becomes.
 *
 * hprof-analyzer:
 *   hprof-analyzer heap.hprof report.html --collections
 *   → HashMap with capacity 1 048 576 and size 100 flagged as sparse
 *   hprof-analyzer heap query heap.hprof \
 *       --oql "SELECT h, h.size, h.table.@length AS cap,
 *                      h.size * 100 / h.table.@length AS fill_pct
 *               FROM java.util.HashMap h
 *               WHERE h.table.@length > 1000 ORDER BY fill_pct ASC LIMIT 10"
 *
 * Fix:   After large-scale removal, compact the map:
 *          map = new HashMap<>(map);                      // copy constructor
 *          // or:
 *          map = new HashMap<>(map.size() * 2, 0.75f);   // explicit resize
 *          map.putAll(original);
 *        Consider using a cache library (Caffeine) with maxSize and eviction policy
 *        instead of a raw HashMap with manual removal.
 */
import java.util.*;

public class _19_SparseMapAfterRemoval {

    public static void main(String[] args) throws Exception {
        System.out.println("Phase 1: Warming up cache with 500 000 entries …");
        Map<Integer, byte[]> cache = new HashMap<>();

        for (int i = 0; i < 500_000; i++) {
            cache.put(i, new byte[64]); // 64-byte value per entry
        }
        Runtime rt = Runtime.getRuntime();
        System.out.printf("  After fill:   size=%,d  table.length≈%,d  heap=%,d MB%n",
            cache.size(), nextPow2(cache.size()),
            (rt.totalMemory() - rt.freeMemory()) >> 20);

        System.out.println("Phase 2: Expiring 99.98% of entries (simulating TTL expiry) …");
        // Keep only entries with id < 100
        cache.entrySet().removeIf(e -> e.getKey() >= 100);

        System.gc();
        Thread.sleep(300);
        System.out.printf("  After removal: size=%,d  table.length≈%,d  heap=%,d MB%n",
            cache.size(), nextPow2(500_000),  // table STAYS at peak size
            (rt.totalMemory() - rt.freeMemory()) >> 20);
        System.out.println("  Fill ratio: ~" + cache.size() + " / " + nextPow2(500_000)
            + " = " + String.format("%.4f%%", cache.size() * 100.0 / nextPow2(500_000)));
        System.out.println();

        // ✅ FIX: compact after large removal
        System.out.println("Phase 3 (FIX): Compacting the map …");
        Map<Integer, byte[]> compacted = new HashMap<>(cache);
        cache = null;
        System.gc();
        Thread.sleep(300);
        System.out.printf("  After compact: size=%,d  table.length≈%,d  heap=%,d MB%n",
            compacted.size(), nextPow2(compacted.size()),
            (rt.totalMemory() - rt.freeMemory()) >> 20);

        System.out.println();
        System.out.println("Take a heap dump of the sparse state: re-run and dump before phase 3.");
        System.out.println("  hprof-analyzer heap.hprof report.html --collections");

        // Keep sparse map alive for heap dump
        Map<Integer, byte[]> sparse = new HashMap<>();
        for (int i = 0; i < 500_000; i++) sparse.put(i, new byte[64]);
        sparse.entrySet().removeIf(e -> e.getKey() >= 100);
        System.out.println("\nSparse map kept alive. Dump now:");
        System.out.println("  jcmd $(pgrep -f SparseMapAfterRemoval) GC.heap_dump heap.hprof");
        Thread.sleep(Long.MAX_VALUE);
    }

    static int nextPow2(int n) {
        int p = 1;
        while (p < n) p <<= 1;
        return p;
    }
}

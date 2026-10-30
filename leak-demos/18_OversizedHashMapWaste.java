/**
 * SCENARIO 18 — Oversized HashMap: pessimistic initial capacity wastes the bucket table
 *
 * Run:   java -Xmx256m 18_OversizedHashMapWaste.java
 *        (No OOM — this is a WASTE scenario, not a leak. Heap dump mid-run.)
 *
 * Capture:  jcmd $(pgrep -f OversizedHashMapWaste) GC.heap_dump heap.hprof
 *
 * Root:  HashMap internally stores entries in a Node[] table whose length is always
 *        a power of two ≥ initialCapacity.  When initialCapacity is set to a large
 *        value but only a few entries are added, most of the table[] slots are null —
 *        wasted heap, invisible from the outside.
 *
 *        Sizing formula: table.length = nextPowerOfTwo( max(initialCapacity, size/loadFactor) )
 *        So new HashMap<>(10_000) allocates table[16384] = 16384 × 4 bytes = 64 KB of
 *        pure waste when you only ever put 3 entries in it.
 *
 *        Real-world trigger: config maps created once per request/thread/bean with a
 *        pessimistic default capacity, or a copy of a large map that shrunk later.
 *        When you have 50 000 such maps (one per HTTP session, per parsed record, etc.)
 *        the waste multiplies: 50 000 × 64 KB = 3 GB.
 *
 * hprof-analyzer:
 *   hprof-analyzer heap.hprof report.html --collections
 *   → "Collection fill ratios" section shows HashMaps with capacity >> size
 *   hprof-analyzer heap query heap.hprof \
 *       --oql "SELECT h, h.size, h.table.@length AS capacity FROM java.util.HashMap h
 *               WHERE h.table.@length > 100 ORDER BY h.table.@length DESC LIMIT 10"
 *
 * Fix:   Size for actual expected entries, accounting for the load factor:
 *          new HashMap<>( (int)(expectedEntries / 0.75) + 1 )
 *        Or use Map.of() / Map.copyOf() for small fixed maps — they use compact storage.
 *        After bulk removals, recreate the map to reclaim the table:
 *          map = new HashMap<>(map);  // copy constructor sizes to actual content
 */
import java.util.*;

public class _18_OversizedHashMapWaste {

    // Simulates a session-scoped attribute store — created once per user session
    static class SessionAttributes {
        // ❌ BAD: pessimistic capacity — most sessions only ever set 2-3 attributes
        final Map<String, Object> attrs = new HashMap<>(1_000);

        // ✅ FIX: lazy init or realistic sizing
        // final Map<String, Object> attrs = new HashMap<>(8);

        SessionAttributes(int sessionId) {
            // Typical session: only a handful of attributes
            attrs.put("userId", sessionId);
            attrs.put("role",   "user");
            // 998 table slots wasted per session
        }
    }

    // Simulates a per-record feature map from a batch job
    static Map<String, Double> buildFeatureMap(int recordId) {
        // ❌ BAD: "just in case" capacity — actual feature count is always ≤ 5
        Map<String, Double> features = new HashMap<>(512);
        features.put("score",    Math.random());
        features.put("weight",   Math.random());
        features.put("priority", (double) (recordId % 10));
        return features;
        // 509 table slots wasted per record
    }

    public static void main(String[] args) throws Exception {
        int N = 20_000;
        List<SessionAttributes> sessions = new ArrayList<>(N);
        List<Map<String, Double>> features = new ArrayList<>(N);

        System.out.println("Creating " + N + " oversized HashMaps …");
        System.out.println("  Each map: capacity=1024 (next power of 2 above 1000), actual entries=2");
        System.out.println("  Each feature: capacity=512, actual entries=3");
        System.out.println("  Total wasted table[] entries ≈ " + (N * (1022 + 509)) + " slots");
        System.out.println();

        for (int i = 0; i < N; i++) {
            sessions.add(new SessionAttributes(i));
            features.add(buildFeatureMap(i));
        }

        Runtime rt = Runtime.getRuntime();
        System.gc();
        Thread.sleep(200);
        System.out.printf("Heap used: %,d MB / %,d MB%n",
            (rt.totalMemory() - rt.freeMemory()) >> 20, rt.maxMemory() >> 20);
        System.out.println();
        System.out.println("Take a heap dump now:");
        System.out.println("  jcmd $(pgrep -f OversizedHashMapWaste) GC.heap_dump heap.hprof");
        System.out.println("Then analyze:");
        System.out.println("  hprof-analyzer heap.hprof report.html --collections");

        Thread.sleep(Long.MAX_VALUE); // keep alive for heap dump
    }
}

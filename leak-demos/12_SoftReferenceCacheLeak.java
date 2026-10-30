/**
 * SCENARIO 12 — SoftReference cache exhausts the heap just before OOM
 *
 * Run:   java -Xmx64m -XX:+HeapDumpOnOutOfMemoryError -XX:HeapDumpPath=heap.hprof \
 *             12_SoftReferenceCacheLeak.java
 *
 * OOM:   java.lang.OutOfMemoryError: Java heap space
 *
 * Root:  SoftReferences are cleared by the GC "before" an OutOfMemoryError is thrown
 *        — but only when memory is *truly* tight. On a machine with sufficient heap,
 *        the GC will keep soft referents alive for a long time (the HotSpot policy is
 *        roughly: clear a SoftReference if its referent has not been accessed for
 *        more than (heap-free-in-MB × SoftRefLRUPolicyMSPerMB) milliseconds).
 *
 *        If the combined size of all soft-referenced objects exceeds the heap AND new
 *        allocations keep arriving faster than the GC can clear soft refs, OOM occurs.
 *        The GC pauses become very long ("GC overhead limit exceeded" may also fire).
 *
 *        Common mistake: using SoftReference as a "free" cache — assuming the GC will
 *        clean it up before OOM. It does, but not until memory pressure is extreme,
 *        and the clearing is non-deterministic.
 *
 * Heap dump:
 *   hprof-analyzer heap report heap.hprof --section leaks
 *   → Soft-referenced byte[] arrays (the cache values) dominate the heap.
 *   hprof-analyzer heap query heap.hprof \
 *       --oql "SELECT COUNT(*) FROM java.lang.ref.SoftReference"
 *
 * Fix:   Use a hard-capped cache (Caffeine with maximumWeight or maximumSize) rather
 *        than relying on SoftReference semantics for memory management.
 *        Keep SoftReference only for "nice-to-have" re-computation savings, not for
 *        correctness or bounding memory usage.
 *
 * Tuning knob (not a fix, just understanding):
 *   -XX:SoftRefLRUPolicyMSPerMB=0   → clear SoftRefs aggressively (default: 1000 ms/MB)
 */
import java.lang.ref.*;
import java.util.*;

public class _12_SoftReferenceCacheLeak {

    static final Map<String, SoftReference<byte[]>> CACHE = new LinkedHashMap<>();

    static byte[] get(String key) {
        SoftReference<byte[]> ref = CACHE.get(key);
        return (ref != null) ? ref.get() : null;
    }

    static void put(String key, byte[] value) {
        CACHE.put(key, new SoftReference<>(value));
    }

    public static void main(String[] args) throws Exception {
        System.out.println("Filling SoftReference cache … (OOM or severe GC pressure expected)");
        System.out.println("  Add -XX:SoftRefLRUPolicyMSPerMB=0 to see refs cleared more aggressively.");

        for (int i = 0; ; i++) {
            String key = "image-" + i;
            put(key, new byte[100_000]); // 100 KB per "cached image"

            // Access recent entries to keep them "warm" (prevent LRU-based clearing)
            if (i > 10) get("image-" + (i - 5));

            if (i % 1_000 == 0) {
                long live = CACHE.values().stream().filter(r -> r.get() != null).count();
                Runtime rt = Runtime.getRuntime();
                System.out.printf("  entries=%,6d  live=%,6d  used=%,d MB / max=%,d MB%n",
                    CACHE.size(), live,
                    (rt.totalMemory() - rt.freeMemory()) >> 20,
                    rt.maxMemory() >> 20);
            }
        }
    }
}

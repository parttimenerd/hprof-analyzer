/**
 * SCENARIO 01 — Static field cache with no eviction policy
 *
 * Run:   java -Xmx64m -XX:+HeapDumpOnOutOfMemoryError -XX:HeapDumpPath=heap.hprof \
 *             01_StaticCacheLeak.java
 *
 * OOM:   java.lang.OutOfMemoryError: Java heap space
 *
 * Root:  CACHE is declared static, making it a GC root.
 *        Objects put into it can never be collected — the map grows forever.
 *        This is the simplest and most common Java leak: "I stored it once and
 *        forgot about it."
 *
 * Heap dump:
 *   hprof-analyzer heap report heap.hprof --section leaks
 *   → CRITICAL: Single object (CACHE HashMap) retains ~100% of heap.
 *   hprof-analyzer heap query heap.hprof \
 *       --oql "SELECT m, m.size FROM java.util.HashMap m ORDER BY m.size DESC LIMIT 5"
 *
 * Fix:   Replace HashMap with a bounded cache. Options (no external deps):
 *          // LRU cache via LinkedHashMap, max 1000 entries:
 *          new LinkedHashMap<>(16, 0.75f, true) {
 *              protected boolean removeEldestEntry(Map.Entry e) { return size() > 1000; }
 *          };
 *        Or use Caffeine (recommended): Caffeine.newBuilder().maximumSize(1000).build()
 */
import java.util.*;

public class _01_StaticCacheLeak {

    // ❌ BAD: unbounded static cache — every entry lives until JVM exit
    static final Map<String, byte[]> CACHE = new HashMap<>();

    static void handleRequest(String requestId) {
        // Simulates caching a "session" or "computed result" per request
        CACHE.put(requestId, new byte[10_000]); // 10 KB per entry — never evicted
    }

    public static void main(String[] args) {
        System.out.println("Filling static cache. Watch heap grow …");
        for (int i = 0; ; i++) {
            handleRequest("session-" + i);
            if (i % 5_000 == 0)
                System.out.printf("  entries=%,8d  heap=%,d / %,d MB%n",
                    CACHE.size(),
                    (Runtime.getRuntime().totalMemory() - Runtime.getRuntime().freeMemory()) >> 20,
                    Runtime.getRuntime().maxMemory() >> 20);
        }
    }
}

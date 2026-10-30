/**
 * SCENARIO 28 — String.intern() abuse: unique strings fill the JVM string table
 *
 * Run:   java -Xmx128m -XX:StringTableSize=65537 28_StringInternAbuse.java
 *        (May OOM or cause severe GC pressure)
 *
 * Root:  String.intern() returns a canonical reference from the JVM's string table
 *        (a native hash table). Interned strings live in the heap but are kept
 *        strongly reachable by the JVM string table — they are never collected
 *        as long as the string table holds them (they ARE collected if the string
 *        table entry is cleared, but clearing requires a GC cycle that traverses
 *        the table, which is expensive at large scale).
 *
 *        Correct use: intern a small set of HOT, REPEATED strings to deduplicate them.
 *        Abuse: intern every string you encounter, including unique ones (UUIDs, user IDs,
 *               dynamic keys). Each unique interned string uses heap AND native memory
 *               for the table bucket. At millions of entries, the table becomes a GC
 *               bottleneck because the GC must scan it at every collection.
 *
 *        Two patterns shown:
 *        A) Good intern: repeated status values → real deduplication benefit
 *        B) Bad intern: unique IDs → no deduplication, just permanent heap retention
 *
 * hprof-analyzer:
 *   hprof-analyzer heap histogram heap.hprof
 *   → Large count of java.lang.String — many are the interned unique IDs
 *
 *   hprof-analyzer heap query heap.hprof \
 *       --oql "SELECT COUNT(*) FROM java.lang.String"
 *
 *   # Interned strings are NOT visible as a GC root chain in the dump;
 *   # they appear as regular String objects. The string table is a native
 *   # structure — use -Xlog:stringtable*=debug to observe it live:
 *   #   jcmd <pid> VM.stringtable  (prints size + occupancy)
 *
 *   # Check for intern abuse heuristic:
 *   hprof-analyzer heap query heap.hprof \
 *       --oql "SELECT toString(s) AS val, COUNT(*) AS n
 *               FROM java.lang.String s
 *               GROUP BY toString(s) HAVING COUNT(*) = 1
 *               ORDER BY n DESC LIMIT 10"
 *   → All unique strings (n=1) that were individually interned are a waste indicator
 *
 * Fix:
 *   (a) Never intern dynamically generated / unique strings.
 *   (b) Use a bounded Map<String, String> pool instead of intern() for controlled dedup:
 *         private static final Map<String,String> POOL = new ConcurrentHashMap<>(1024);
 *         return POOL.computeIfAbsent(raw, s -> s); // evict with expiry if needed
 *   (c) Use enum for a truly fixed set of values.
 *   (d) Monitor the string table: jcmd <pid> VM.stringtable
 */
public class _28_StringInternAbuse {

    static String[] GOOD_VALUES = {"ACTIVE", "INACTIVE", "PENDING", "DELETED", "SUSPENDED"};

    // Pattern A: CORRECT intern usage — deduplicate a small set of hot strings
    static void patternA_GoodIntern(int n) {
        String[] results = new String[n];
        for (int i = 0; i < n; i++) {
            // Each call with the same content returns the SAME interned object
            results[i] = GOOD_VALUES[i % GOOD_VALUES.length].intern();
        }
        System.out.printf("  Pattern A (good): %,d calls → ~%d unique interned strings%n",
            n, GOOD_VALUES.length);
        System.out.println("  " + results[0]); // prevent optimizer
    }

    // Pattern B: WRONG intern usage — unique strings, zero deduplication benefit
    static String[] SINK; // keep interned strings alive

    static void patternB_BadIntern(int n) {
        SINK = new String[n];
        for (int i = 0; i < n; i++) {
            // Each unique string produces a new string table entry — permanent retention
            String uid = "user-" + i + "@org.example.com"; // unique every iteration
            SINK[i] = uid.intern(); // ❌ no benefit — uid never appears again
        }
        System.out.printf("  Pattern B (bad):  %,d calls → %,d unique entries in string table%n",
            n, n);
        System.out.println("  " + SINK[0]);
    }

    public static void main(String[] args) throws Exception {
        System.out.println("Pattern A: intern a small set of repeated values (correct usage) …");
        patternA_GoodIntern(1_000_000);

        System.out.println("Pattern B: intern unique strings (abuse — fills string table) …");
        patternB_BadIntern(500_000);

        Runtime rt = Runtime.getRuntime();
        System.gc(); Thread.sleep(300);
        System.out.printf("%nHeap used: %,d MB / %,d MB%n",
            (rt.totalMemory() - rt.freeMemory()) >> 20, rt.maxMemory() >> 20);

        System.out.println("\nCheck string table occupancy:");
        System.out.println("  jcmd $(pgrep -f StringInternAbuse) VM.stringtable");
        System.out.println("\nTake a heap dump:");
        System.out.println("  jcmd $(pgrep -f StringInternAbuse) GC.heap_dump heap.hprof");
        System.out.println("Then: hprof-analyzer heap histogram heap.hprof");

        Thread.sleep(Long.MAX_VALUE);
    }
}

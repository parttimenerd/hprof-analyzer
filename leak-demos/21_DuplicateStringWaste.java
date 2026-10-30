/**
 * SCENARIO 21 — Duplicate String values: many String objects with identical content
 *
 * Run:   java -Xmx256m 21_DuplicateStringWaste.java
 *        (No OOM — WASTE scenario. Heap dump mid-run.)
 *
 * Capture:  jcmd $(pgrep -f DuplicateStringWaste) GC.heap_dump heap.hprof
 *
 * Root:  Every String literal reuses the same interned object, but Strings constructed
 *        at runtime (from parsing, substring, concatenation, deserialization) create a
 *        brand-new heap object even if the content is identical to an existing String.
 *
 *        This scenario models three realistic sources:
 *
 *        A) Status / enum-like fields: a JSON deserializer creates a new String("ACTIVE")
 *           for every record that has status=ACTIVE. With 500K records, that is 500K
 *           separate String objects, each containing char[] {'A','C','T','I','V','E'},
 *           even though a single interned String would suffice.
 *
 *        B) Field names: a schema-less parser creates new String("firstName"),
 *           String("lastName"), etc. for every parsed object's keys.
 *
 *        C) Repeated error messages / log tokens stored in collections.
 *
 * hprof-analyzer:
 *   hprof-analyzer heap.hprof report.html --find-duplicates
 *   → "Duplicate Strings" section: top offenders sorted by wasted bytes
 *   hprof-analyzer heap query heap.hprof \
 *       --oql "SELECT toString(s) AS val, COUNT(*) AS n
 *               FROM java.lang.String s
 *               GROUP BY toString(s) HAVING COUNT(*) > 100
 *               ORDER BY n DESC LIMIT 20"
 *
 * Fix:
 *   (a) Use enum instead of String for bounded value sets (status, type, role).
 *   (b) Intern hot strings at parse time:
 *         String status = rawValue.intern(); // or a custom intern map
 *   (c) Use a bounded intern cache (Guava Interner, or a simple ConcurrentHashMap<String,String>):
 *         private static final Map<String,String> POOL = new ConcurrentHashMap<>();
 *         String deduplicated = POOL.computeIfAbsent(raw, Function.identity());
 *   (d) Enable JVM string deduplication (G1/ZGC only, heap strings, not compile-time constants):
 *         -XX:+UseStringDeduplication
 *
 * See also: JEP 192 (String Deduplication in G1)
 */
import java.util.*;

public class _21_DuplicateStringWaste {

    // Simulates a deserialized record — fields created by a JSON parser at runtime
    static class Record {
        final String id;
        final String status;     // one of ~5 possible values, always new String(...)
        final String category;   // one of ~20 possible values
        final String region;     // one of ~8 possible values
        final String[] tags;     // 2-4 per record, repeated across records

        Record(int i) {
            this.id       = "rec-" + i;
            // ❌ BAD: new String() constructed from a char array or concatenation
            //         at parse time — NOT interned, even if value is repeated
            this.status   = new String(STATUS_VALUES[i % STATUS_VALUES.length]);
            this.category = new String(CATEGORY_VALUES[i % CATEGORY_VALUES.length]);
            this.region   = new String(REGION_VALUES[i % REGION_VALUES.length]);
            this.tags     = new String[]{
                new String(TAG_VALUES[i % TAG_VALUES.length]),
                new String(TAG_VALUES[(i + 1) % TAG_VALUES.length])
            };

            // ✅ FIX: intern or use enum
            // this.status   = STATUS_VALUES[i % STATUS_VALUES.length];  // reuse existing String
            // this.category = CATEGORY_VALUES[i % CATEGORY_VALUES.length];
        }
    }

    // These constants exist as interned strings — but the Record constructor
    // creates NEW String objects from their char[] contents instead of reusing them.
    static final String[] STATUS_VALUES   = {"ACTIVE", "INACTIVE", "PENDING", "SUSPENDED", "DELETED"};
    static final String[] CATEGORY_VALUES = {"electronics","clothing","food","books","sports",
                                             "toys","furniture","beauty","automotive","garden",
                                             "office","music","art","outdoor","health",
                                             "kids","pets","tools","kitchen","games"};
    static final String[] REGION_VALUES   = {"us-east","us-west","eu-central","eu-north",
                                             "ap-south","ap-east","sa-east","af-south"};
    static final String[] TAG_VALUES      = {"sale","new","featured","trending","bestseller",
                                             "limited","exclusive","refurbished","organic","premium"};

    public static void main(String[] args) throws Exception {
        int N = 500_000;
        System.out.println("Creating " + N + " records with duplicated runtime Strings …");
        System.out.println("  STATUS_VALUES has " + STATUS_VALUES.length + " distinct values");
        System.out.println("  Expected: ~" + N + " duplicate 'status' strings, ~"
            + (N / STATUS_VALUES.length) + " copies of each value");

        List<Record> records = new ArrayList<>(N);
        for (int i = 0; i < N; i++) {
            records.add(new Record(i));
            if (i % 100_000 == 0) System.out.printf("  %,d records …%n", i);
        }

        Runtime rt = Runtime.getRuntime();
        System.gc(); Thread.sleep(300);
        System.out.printf("%nHeap used: %,d MB / %,d MB%n",
            (rt.totalMemory() - rt.freeMemory()) >> 20, rt.maxMemory() >> 20);
        System.out.println();
        System.out.println("Take a heap dump now:");
        System.out.println("  jcmd $(pgrep -f DuplicateStringWaste) GC.heap_dump heap.hprof");
        System.out.println("Then: hprof-analyzer heap.hprof report.html --find-duplicates");

        System.out.println(records.get(0).status); // prevent optimizer from eliminating records
        Thread.sleep(Long.MAX_VALUE);
    }
}

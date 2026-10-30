/**
 * SCENARIO 26 — Boxed primitives: List<Integer> vs int[] for large numeric datasets
 *
 * THIS IS NOT A BUG per se — using List<Integer> is idiomatic Java. But it
 * demonstrates a 4-6x memory overhead over int[] that becomes significant at scale.
 *
 * Run:   java -Xmx256m 26_BoxedPrimitivesWaste.java
 *        (No OOM intended. Heap dump mid-run.)
 *
 * Capture:  jcmd $(pgrep -f BoxedPrimitivesWaste) GC.heap_dump heap.hprof
 *
 * Root:  A boxed Integer object occupies ~16 bytes (12-byte header + 4-byte value,
 *        rounded to 16 due to alignment). A plain int occupies 4 bytes.
 *        A List<Integer> with 1M elements therefore uses:
 *          - Object[] elementData: 1M refs × 4 bytes (compressed oops) = 4 MB
 *          - 1M Integer objects: 1M × 16 bytes                          = 16 MB
 *          - Total: ~20 MB
 *        vs int[] with 1M elements: 1M × 4 bytes = 4 MB.
 *        Ratio: 5x.
 *
 *        The heap dump shows:
 *          - A large Object[] (the ArrayList's elementData)
 *          - A huge count of java.lang.Integer instances
 *          - Most Integer values are unique, so Integer cache (-128 to 127) doesn't help
 *
 *        Real-world: metric IDs, user IDs, event counts, time buckets, histogram bins —
 *        all stored in ArrayList<Integer> when the developer didn't notice the overhead.
 *
 * hprof-analyzer:
 *   hprof-analyzer heap histogram heap.hprof
 *   → java.lang.Integer: millions of instances, large total shallow size
 *
 *   hprof-analyzer heap query heap.hprof \
 *       --oql "SELECT COUNT(*) AS n FROM java.lang.Integer"
 *
 *   hprof-analyzer heap query heap.hprof \
 *       --oql "SELECT @displayName, COUNT(*) AS n, SUM(@usedHeapSize) AS total
 *               FROM INSTANCEOF java.lang.Number
 *               GROUP BY @displayName ORDER BY total DESC LIMIT 10"
 *
 * Fix:
 *   (a) Use int[] or long[] directly when the collection is effectively final.
 *   (b) Use Eclipse Collections IntList, LongList, or similar primitive containers.
 *   (c) Use IntStream / LongStream pipelines instead of List<Integer>.
 *   (d) Store metrics as parallel primitive arrays: int[] ids, double[] values.
 *   Note: Project Valhalla (JEP 401+) aims to eliminate this overhead for value types.
 */
import java.util.*;

public class _26_BoxedPrimitivesWaste {

    public static void main(String[] args) throws Exception {
        int N = 2_000_000; // 2M numeric values

        System.out.printf("Allocating %,d integers in two ways:%n%n", N);

        // ❌ BAD: List<Integer> — each element is a separate heap object
        System.out.println("  List<Integer> (boxed) …");
        List<Integer> boxedList = new ArrayList<>(N);
        for (int i = 0; i < N; i++) {
            boxedList.add(i + 1000); // values > 127 → NOT cached → fresh Integer each time
        }

        Runtime rt = Runtime.getRuntime();
        System.gc(); Thread.sleep(200);
        long heapAfterBoxed = rt.totalMemory() - rt.freeMemory();
        System.out.printf("    Heap used: %,d MB%n", heapAfterBoxed >> 20);

        // ✅ GOOD: int[] — contiguous, no object overhead
        System.out.println("  int[] (primitive) …");
        int[] primitiveArray = new int[N];
        for (int i = 0; i < N; i++) {
            primitiveArray[i] = i + 1000;
        }

        System.gc(); Thread.sleep(200);
        long heapAfterBoth = rt.totalMemory() - rt.freeMemory();
        System.out.printf("    Heap used: %,d MB (int[] added ~%,d MB)%n",
            heapAfterBoth >> 20,
            (heapAfterBoth - heapAfterBoxed) >> 20);

        System.out.println();
        System.out.printf("  List<Integer>: ~%,d MB for %,d ints%n",
            (long) N * 20 / 1024 / 1024, N); // ~20 bytes per boxed int
        System.out.printf("  int[]:         ~%,d MB for %,d ints%n",
            (long) N * 4 / 1024 / 1024, N);   // 4 bytes per primitive int
        System.out.printf("  Overhead ratio: ~%.1fx%n", 20.0 / 4.0);
        System.out.println();
        System.out.println("Take a heap dump now:");
        System.out.println("  jcmd $(pgrep -f BoxedPrimitivesWaste) GC.heap_dump heap.hprof");
        System.out.println("Then: hprof-analyzer heap histogram heap.hprof");

        System.out.println(boxedList.get(0) + " " + primitiveArray[0]); // prevent optimizer
        Thread.sleep(Long.MAX_VALUE);
    }
}

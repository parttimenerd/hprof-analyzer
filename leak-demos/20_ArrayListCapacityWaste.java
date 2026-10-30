/**
 * SCENARIO 20 — ArrayList with excess capacity: elementData[] is much larger than size
 *
 * Run:   java -Xmx128m 20_ArrayListCapacityWaste.java
 *        (No OOM — WASTE scenario. Heap dump mid-run.)
 *
 * Capture:  jcmd $(pgrep -f ArrayListCapacityWaste) GC.heap_dump heap.hprof
 *
 * Root:  ArrayList stores elements in an Object[] elementData.  The capacity
 *        (elementData.length) can far exceed the logical size when:
 *          (a) Created with a large initial capacity:  new ArrayList<>(100_000)
 *          (b) Grown via ensureCapacity(n) then not filled
 *          (c) Many elements added then removed (clear() keeps the full capacity)
 *
 *        A list with size=2 and capacity=131072 wastes 131070 reference slots
 *        (≈ 512 KB per list on a 64-bit JVM with compressed oops).
 *
 * Patterns shown:
 *   A) Pre-allocated list filled only partially
 *   B) List after clear() — capacity retained, size = 0
 *   C) Map whose values are ArrayLists — nearly all empty
 *
 * hprof-analyzer:
 *   hprof-analyzer heap.hprof report.html --collections
 *   → Lists with fill ratio < 10% or size=0 highlighted
 *   hprof-analyzer heap query heap.hprof \
 *       --oql "SELECT l, l.size, l.elementData.@length AS cap
 *               FROM java.util.ArrayList l
 *               WHERE l.elementData.@length > 100 ORDER BY cap DESC LIMIT 10"
 *
 * Fix:
 *   (a) Don't over-specify initial capacity unless you KNOW the size upfront.
 *   (b) After clearing a list you plan to reuse small: list.trimToSize()
 *   (c) For maps of lists, create the inner list lazily (on first add) rather than eagerly.
 *   (d) Replace ArrayList<Integer> with IntArrayList (e.g. Eclipse Collections) for large
 *       numeric lists — avoids boxing overhead too.
 */
import java.util.*;

public class _20_ArrayListCapacityWaste {

    public static void main(String[] args) throws Exception {
        int N = 10_000;

        // Pattern A: Pre-allocated with large capacity, sparse fill
        System.out.println("Pattern A: Pre-allocated lists with large capacity, few elements …");
        List<List<String>> preAllocated = new ArrayList<>(N);
        for (int i = 0; i < N; i++) {
            // ❌ BAD: assume "up to 1000 tags" but most items have ≤3
            List<String> tags = new ArrayList<>(1_000);
            tags.add("category:" + (i % 20));
            if (i % 3 == 0) tags.add("featured");
            preAllocated.add(tags);
            // capacity=1024 (next pow2), size=1 or 2 → ~99.8% waste
        }

        // Pattern B: Lists after clear() — capacity retained
        System.out.println("Pattern B: Lists after clear() retain full capacity …");
        List<List<Object>> cleared = new ArrayList<>(N);
        for (int i = 0; i < N; i++) {
            List<Object> buf = new ArrayList<>(256);
            for (int j = 0; j < 200; j++) buf.add(new Object()); // fill it up
            buf.clear(); // ❌ BAD: capacity=256 kept, size=0
            // ✅ FIX: buf.trimToSize() after clear, or new ArrayList<>()
            cleared.add(buf);
        }

        // Pattern C: Map with empty-list values (common in groupBy results)
        System.out.println("Pattern C: Map<K, List<V>> where most lists are empty …");
        Map<Integer, List<String>> groupByCategory = new HashMap<>();
        // Pre-populate ALL possible categories eagerly
        for (int cat = 0; cat < 1_000; cat++) {
            groupByCategory.put(cat, new ArrayList<>(100)); // ❌ eagerly pre-allocated
        }
        // Only a few categories actually get items
        for (int i = 0; i < 50; i++) {
            groupByCategory.get(i % 10).add("item-" + i);
        }
        // 990 out of 1000 lists are empty, each wasting 100-slot capacity

        Runtime rt = Runtime.getRuntime();
        System.gc();
        Thread.sleep(200);
        System.out.printf("%nHeap used: %,d MB / %,d MB%n",
            (rt.totalMemory() - rt.freeMemory()) >> 20, rt.maxMemory() >> 20);
        System.out.println();
        System.out.println("Take a heap dump now:");
        System.out.println("  jcmd $(pgrep -f ArrayListCapacityWaste) GC.heap_dump heap.hprof");
        System.out.println("Then: hprof-analyzer heap.hprof report.html --collections");

        // Prevent GC
        System.out.println(preAllocated.size() + " " + cleared.size() + " " + groupByCategory.size());
        Thread.sleep(Long.MAX_VALUE);
    }
}

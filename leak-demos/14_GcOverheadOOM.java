/**
 * SCENARIO 14 — GC overhead limit exceeded
 *
 * Run:   java -Xmx64m -XX:+HeapDumpOnOutOfMemoryError -XX:HeapDumpPath=heap.hprof \
 *             14_GcOverheadOOM.java
 *
 * OOM:   java.lang.OutOfMemoryError: GC overhead limit exceeded
 *
 * Root:  The JVM throws this error when the GC is spending more than 98% of CPU
 *        time but is recovering less than 2% of the heap per GC cycle. This is
 *        distinct from "Java heap space": the heap is not yet full, but GC is
 *        running so continuously that the application makes no forward progress.
 *
 *        To trigger this reliably we:
 *        (a) Fill ~90% of the heap with objects that stay live (the "floor")
 *        (b) Continuously allocate small short-lived objects that force frequent
 *            GC cycles, but the live floor leaves so little free space that each
 *            cycle reclaims almost nothing.
 *
 *        Real-world cause: a large live set (e.g., a bloated cache) combined with
 *        high allocation rate. The app "works" until the cache grows beyond a threshold,
 *        then it stalls in GC.
 *
 *        Note: This error can be disabled with -XX:-UseGCOverheadLimit (not recommended).
 *
 * Heap dump:
 *   hprof-analyzer heap report heap.hprof --section triage
 *   → CRITICAL or HIGH signals for objects retaining ~90% of heap.
 *   hprof-analyzer heap report heap.hprof --section leaks
 *   → The "floor" collection appears as the top suspect.
 */
import java.util.*;

public class _14_GcOverheadOOM {

    public static void main(String[] args) throws Exception {
        System.out.println("Building live-set floor then thrashing GC … (GC overhead OOM expected)");

        // Phase 1: fill ~85% of the heap with permanently-live objects
        List<byte[]> floor = new ArrayList<>();
        Runtime rt = Runtime.getRuntime();
        long target = (long) (rt.maxMemory() * 0.85);
        while ((rt.totalMemory() - rt.freeMemory()) < target) {
            floor.add(new byte[4_096]); // 4 KB chunks
        }
        System.out.printf("  Floor built: %,d MB live, %,d MB max%n",
            (rt.totalMemory() - rt.freeMemory()) >> 20, rt.maxMemory() >> 20);

        // Phase 2: allocate tiny short-lived objects at high rate
        // GC fires constantly but can only reclaim the ~15% free — nearly nothing per cycle
        System.out.println("  Thrashing allocator …");
        long count = 0;
        while (true) {
            @SuppressWarnings("unused")
            byte[] tmp = new byte[128]; // immediately unreachable → garbage
            count++;
            if (count % 500_000 == 0)
                System.out.printf("  allocations=%,d  used=%,d MB%n",
                    count, (rt.totalMemory() - rt.freeMemory()) >> 20);
        }
    }
}

/**
 * SCENARIO 10 — Finalizer queue backlog
 *
 * Run:   java -Xmx128m -XX:+HeapDumpOnOutOfMemoryError -XX:HeapDumpPath=heap.hprof \
 *             10_FinalizerQueueLeak.java
 *
 * OOM:   java.lang.OutOfMemoryError: Java heap space
 *
 * Root:  When an object has a finalize() method, the GC does NOT reclaim it
 *        immediately at collection time. Instead it queues a java.lang.ref.Finalizer
 *        wrapper around the object onto the Finalizer thread's reference queue.
 *        The single Finalizer thread processes this queue one entry at a time.
 *        If finalize() is slow (or blocks), the queue backs up — both the wrapper
 *        objects AND their referents (the BigObject instances) remain on the heap
 *        indefinitely, even though no application code holds references to them.
 *
 *        The heap dump shows:
 *          java.lang.ref.Finalizer  ← wrappers in the queue, one per queued object
 *            └→ referent: BigObject  ← the "freed" objects that aren't freed yet
 *
 * Heap dump:
 *   hprof-analyzer heap report heap.hprof --section leaks
 *   → Suspect: java.lang.ref.Finalizer  (queue backlog)
 *       Dominated objects: BigObject × N  (each 500 KB)
 *   hprof-analyzer heap query heap.hprof \
 *       --oql "SELECT COUNT(*) FROM java.lang.ref.Finalizer"
 *
 * Fix:   Remove finalize() — it is deprecated since Java 9, removed in later versions.
 *        Use java.lang.ref.Cleaner (Java 9+) or try-with-resources for resource cleanup.
 *        If you must use finalize(), keep it as fast as possible (no I/O, no locks).
 */
public class _10_FinalizerQueueLeak {

    static class BigObject {
        final byte[] data = new byte[500_000]; // 500 KB
        static volatile long totalFinalized = 0;

        // ❌ BAD: slow finalize() stalls the Finalizer thread
        @Override
        @SuppressWarnings("deprecation")
        protected void finalize() throws Throwable {
            Thread.sleep(5); // simulate slow cleanup (e.g., I/O, logging)
            totalFinalized++;
            super.finalize();
        }

        // ✅ FIX: use Cleaner instead (shown conceptually)
        // static final java.lang.ref.Cleaner CLEANER = java.lang.ref.Cleaner.create();
        // final java.lang.ref.Cleaner.Cleanable cleanable;
        // BigObject() {
        //     cleanable = CLEANER.register(this, () -> {
        //         // fast, non-blocking cleanup here
        //     });
        // }
    }

    public static void main(String[] args) throws Exception {
        System.out.println("Producing finalizable objects faster than Finalizer can process … (OOM expected)");
        for (long i = 0; ; i++) {
            new BigObject(); // no reference held — "ready for GC" immediately …
                             // … but must wait in the Finalizer queue first

            if (i % 500 == 0) {
                Runtime rt = Runtime.getRuntime();
                System.out.printf("  created=%,7d  finalized=%,7d  queue≈%,d  used=%,d MB%n",
                    i,
                    BigObject.totalFinalized,
                    i - BigObject.totalFinalized,
                    (rt.totalMemory() - rt.freeMemory()) >> 20);
            }
        }
    }
}

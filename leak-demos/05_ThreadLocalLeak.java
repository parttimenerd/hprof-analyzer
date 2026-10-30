/**
 * SCENARIO 05 — ThreadLocal accumulating data on pooled threads
 *
 * Run:   java -Xmx64m -XX:+HeapDumpOnOutOfMemoryError -XX:HeapDumpPath=heap.hprof \
 *             05_ThreadLocalLeak.java
 *
 * OOM:   java.lang.OutOfMemoryError: Java heap space
 *
 * Root:  REQUEST_LOG is a static ThreadLocal<List<byte[]>>. Because it is static,
 *        the ThreadLocal *instance* itself is a GC root — the WeakReference key in
 *        each thread's ThreadLocalMap always has a live referent, so the entry is
 *        never stale-cleaned.
 *
 *        Each call to handleRequest() appends 1 KB to the per-thread list WITHOUT
 *        ever clearing it. On a pool of 4 threads handling 1 M requests:
 *          4 threads × (1 000 000 / 4) requests × 1 KB = 1 GB → OOM.
 *
 *        The subtle variant (non-static ThreadLocal):
 *          When a ThreadLocal instance is non-static, its key in the map is a
 *          WeakReference. Once the ThreadLocal becomes unreachable, GC nulls the key
 *          but the VALUE (the large object) remains in the Entry until the next
 *          set/get/remove call triggers stale-entry expunging. This is the classic
 *          "stale ThreadLocal entry" leak that is hard to see in a heap dump.
 *
 * Heap dump:
 *   hprof-analyzer heap report heap.hprof --section threads
 *   → Each worker thread retains large byte[] arrays via its ThreadLocalMap.
 *   hprof-analyzer heap report heap.hprof --section thread_locals
 *   → Explicit per-thread ThreadLocal values with sizes.
 *
 * Fix:   Always call REQUEST_LOG.remove() in a finally block:
 *          try { handleRequest(id); }
 *          finally { REQUEST_LOG.remove(); }
 *        Or, if you only need fresh state per request, use ThreadLocal.withInitial(...)
 *        and call remove() at the end of each task.
 */
import java.util.*;
import java.util.concurrent.*;

public class _05_ThreadLocalLeak {

    // ❌ BAD: list accumulates without bound on each thread
    static final ThreadLocal<List<byte[]>> REQUEST_LOG =
        ThreadLocal.withInitial(ArrayList::new);

    static final int POOL_SIZE = 4;

    static void handleRequest(int id) {
        REQUEST_LOG.get().add(new byte[1_024]); // 1 KB added to THIS thread's list

        // ✅ FIX: replace the above with:
        // List<byte[]> log = new ArrayList<>();
        // log.add(new byte[1_024]);
        // ... use log locally, never store in ThreadLocal ...
        // OR: call REQUEST_LOG.remove() at the end of this method.

        if (id % 20_000 == 0) {
            List<byte[]> current = REQUEST_LOG.get();
            System.out.printf("  request=%,8d  thread=%s  local-list-size=%,d  approx=%,d KB%n",
                id,
                Thread.currentThread().getName(),
                current.size(),
                current.size()); // 1 KB per entry
        }
    }

    public static void main(String[] args) throws Exception {
        ExecutorService pool = Executors.newFixedThreadPool(POOL_SIZE);
        System.out.printf("Submitting tasks to %d-thread pool without clearing ThreadLocal …%n", POOL_SIZE);
        List<Future<?>> futures = new ArrayList<>();
        for (int i = 0; i < 5_000_000; i++) {
            final int id = i;
            futures.add(pool.submit(() -> handleRequest(id)));
            // Throttle submission to avoid OutOfMemory in the task queue itself
            if (futures.size() >= 2_000) {
                futures.remove(0).get();
            }
        }
        pool.shutdown();
    }
}

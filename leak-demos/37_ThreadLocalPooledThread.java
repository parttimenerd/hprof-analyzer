/**
 * LEAK 37 — ThreadLocal stale values on a reused thread pool
 *
 * A fixed-size thread pool (4 threads) processes tasks that set a ThreadLocal
 * but NEVER call remove(). After pool shutdown the threads remain alive with
 * stale ThreadLocal entries — one RequestContext per pool thread.
 *
 * Run:   java -Xmx64m 37_ThreadLocalPooledThread.java
 *
 * Capture (after "HEAP_DUMP_READY" is printed):
 *   jcmd $(pgrep -f ThreadLocalPooledThread) GC.heap_dump heap.hprof
 *
 * What's in the heap:
 *   4 pool threads (idle after shutdown), each retaining 1 RequestContext
 *   RequestContext: requestId (String), userId (String), payload (byte[1024]),
 *                   timestamp (long)
 *
 * Root cause:
 *   ThreadLocal.set() without a matching remove() leaves the value in the
 *   thread's ThreadLocalMap forever. On short-lived threads this is harmless
 *   (the entry dies with the thread), but pool threads live for the process
 *   lifetime — they accumulate the value from the LAST task that ran on each
 *   thread and never release it.
 *
 * Fix:
 *   Always call ThreadLocal.remove() in a finally block, or use a
 *   servlet/framework filter that clears all request-scoped ThreadLocals after
 *   each request.
 *
 * Analysis:
 *   hprof-analyzer heap report heap.hprof
 *   → "threadlocal-leak" or "gc-root-type: Thread" triage signals
 *
 *   hprof-analyzer heap query heap.hprof \
 *       --oql "SELECT * FROM _37_ThreadLocalPooledThread\$RequestContext"
 *   → Should find exactly 4 instances (one per pool thread)
 */
import java.util.concurrent.*;

public class _37_ThreadLocalPooledThread {

    static class RequestContext {
        final String requestId;
        final String userId;
        final byte[] payload;   // ~1 KB stale data held per pool thread
        final long   timestamp;

        RequestContext(int taskId) {
            this.requestId = "req-" + taskId;
            this.userId    = "user-" + (taskId % 1000);
            this.payload   = new byte[1024];
            this.timestamp = System.currentTimeMillis();
            // Simulate filling payload
            for (int i = 0; i < payload.length; i++) payload[i] = (byte) (taskId ^ i);
        }
    }

    // ❌ BAD: request context set here but never removed
    static final ThreadLocal<RequestContext> REQUEST_CTX = new ThreadLocal<>();

    static void handleTask(int taskId) {
        REQUEST_CTX.set(new RequestContext(taskId));
        // Simulate work (result intentionally unused — demonstrating the leak pattern)
        long sum = 0;
        for (int i = 0; i < 10_000; i++) sum += i;
        // ✅ FIX: REQUEST_CTX.remove();   ← omitted to demonstrate the leak
        if (sum < 0) throw new AssertionError(); // consume sum to satisfy compiler
    }

    public static void main(String[] args) throws Exception {
        int poolSize  = 4;
        int taskCount = 10_000;

        System.out.println("=== ThreadLocal Stale Values on Thread Pool ===");
        System.out.printf("Pool threads: %d, tasks: %,d%n", poolSize, taskCount);

        ExecutorService pool = Executors.newFixedThreadPool(poolSize, r -> {
            Thread t = new Thread(r);
            t.setName("pool-worker");
            t.setDaemon(false);
            return t;
        });

        for (int i = 0; i < taskCount; i++) {
            final int taskId = i;
            pool.submit(() -> handleTask(taskId));
        }

        pool.shutdown();
        pool.awaitTermination(60, TimeUnit.SECONDS);

        System.gc();
        Thread.sleep(200);

        Runtime rt = Runtime.getRuntime();
        System.out.printf("Heap used: %,d KB / %,d KB%n",
            (rt.totalMemory() - rt.freeMemory()) >> 10,
            rt.maxMemory() >> 10);
        System.out.printf("Pool threads with stale TL entries: %d%n", poolSize);
        System.out.println();
        System.out.println("HEAP_DUMP_READY");
        System.out.println("  jcmd $(pgrep -f ThreadLocalPooledThread) GC.heap_dump heap.hprof");
        Thread.sleep(Long.MAX_VALUE);
    }
}

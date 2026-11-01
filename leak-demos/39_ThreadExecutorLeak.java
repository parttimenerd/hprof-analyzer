/**
 * LEAK 39 — Per-request ThreadPoolExecutor leak (200 executors never shut down)
 *
 * Run:   java -Xmx256m 39_ThreadExecutorLeak.java
 *
 * Capture (after "HEAP_DUMP_READY" is printed):
 *   jcmd $(pgrep -f ThreadExecutorLeak) GC.heap_dump heap.hprof
 *
 * Pattern:
 *   A common anti-pattern in services and frameworks: instead of sharing a
 *   single thread pool, each incoming request creates a new ThreadPoolExecutor
 *   (e.g., to isolate per-request parallelism or apply per-customer limits).
 *   The executor is registered in a "service registry" or cached by request ID,
 *   but never shut down or removed. Each executor keeps at least one idle core
 *   thread alive, blocking on the work queue.
 *
 *   Real-world triggers:
 *     - Spring @Async with per-request executor beans that are never destroyed
 *     - CompletableFuture.supplyAsync(task, newExecutor()) without shutdown
 *     - Per-tenant thread pools registered in a map that outlives the tenant session
 *     - Request-scoped executors cached for "reuse" but never evicted
 *
 * What's in the heap:
 *   200 ThreadPoolExecutor instances (each with 1 core thread, 5 max)
 *   200 idle worker threads blocking on LinkedBlockingQueue.take()
 *   200 HandlerState objects (handler ID, config map, start time)
 *   LinkedBlockingQueue per executor (the work queue)
 *
 * Interesting analysis:
 *   hprof-analyzer heap report heap.hprof --section triage
 *   → "Dominant GC-Root Type: Thread" (all retained via idle worker threads)
 *
 *   hprof-analyzer heap histogram heap.hprof
 *   → ThreadPoolExecutor, Thread, LinkedBlockingQueue prominent
 *
 *   hprof-analyzer heap query heap.hprof \
 *       --oql "SELECT COUNT(*) FROM java.util.concurrent.ThreadPoolExecutor"
 *   → 200
 *
 * Fix:
 *   Always call executor.shutdown() (or shutdownNow()) when the logical scope
 *   ends — in a try-finally, or via ExecutorService.close() (Java 19+).
 *   Better: share a bounded pool configured at startup.
 */
import java.util.*;
import java.util.concurrent.*;

public class _39_ThreadExecutorLeak {

    // ❌ BAD: every executor created is retained here; none are ever shut down
    static final List<ThreadPoolExecutor> EXECUTOR_SINK = new ArrayList<>();
    static final List<HandlerState>       RESULTS       = new ArrayList<>();

    static class HandlerState {
        final String              handlerId;
        final long                startTime;
        final Map<String, String> config;

        HandlerState(String id) {
            this.handlerId = id;
            this.startTime = System.currentTimeMillis();
            this.config    = new HashMap<>();
            this.config.put("timeout",    "30000");
            this.config.put("maxRetries", "3");
            this.config.put("region",     "us-east-1");
            this.config.put("tier",       "standard");
            this.config.put("traceId",    UUID.randomUUID().toString());
        }
    }

    static ThreadPoolExecutor createRequestExecutor(String requestId) {
        // ❌ BAD: new executor per request, never shut down
        // ✅ FIX: inject a shared executor; or at minimum call shutdown() in
        //         finally after the request completes
        ThreadPoolExecutor exec = new ThreadPoolExecutor(
            1,                              // corePoolSize  — always 1 idle thread
            5,                              // maximumPoolSize
            60L, TimeUnit.SECONDS,          // keepAliveTime
            new LinkedBlockingQueue<>(100)  // bounded work queue
        );
        exec.allowCoreThreadTimeOut(false); // core thread stays alive indefinitely
        return exec;
    }

    public static void main(String[] args) throws Exception {
        System.out.println("=== Per-Request ThreadPoolExecutor Leak ===");
        System.out.println("Creating 200 request handlers, each with its own executor …");

        for (int i = 0; i < 200; i++) {
            final String requestId = String.format("req-%05d", i);
            ThreadPoolExecutor exec = createRequestExecutor(requestId);

            // Submit one task to force the core thread to start and park on the queue
            Future<String> future = exec.submit(() -> {
                HandlerState state = new HandlerState(requestId);
                RESULTS.add(state); // retain the result in a shared list
                return state.handlerId;
            });

            // Wait for the task to complete so HandlerState is populated,
            // then abandon the executor — it stays registered, core thread alive
            future.get(5, TimeUnit.SECONDS);

            EXECUTOR_SINK.add(exec); // ← the leak: executor never shut down

            if ((i + 1) % 50 == 0) {
                System.out.printf("  created %,3d executors  (active threads: ~%,d)%n",
                    i + 1,
                    EXECUTOR_SINK.stream().mapToInt(ThreadPoolExecutor::getActiveCount).sum()
                        + EXECUTOR_SINK.size() /* each has 1 idle core thread */);
            }
        }

        // Allow the 200 core threads to go idle (parked on queue.take())
        Thread.sleep(500);

        Runtime rt = Runtime.getRuntime();
        System.gc(); Thread.sleep(200);
        System.out.printf("%nHeap: %,d MB used / %,d MB max%n",
            (rt.totalMemory() - rt.freeMemory()) >> 20,
            rt.maxMemory() >> 20);
        System.out.printf("  executors (leaked):  %,d%n", EXECUTOR_SINK.size());
        System.out.printf("  handler results:     %,d%n", RESULTS.size());
        System.out.printf("  live threads (est):  ~%,d%n",
            Thread.activeCount());
        System.out.println();
        System.out.println("HEAP_DUMP_READY");
        System.out.println("  jcmd $(pgrep -f ThreadExecutorLeak) GC.heap_dump heap.hprof");
        Thread.sleep(Long.MAX_VALUE);
    }
}

/**
 * SCENARIO 06 — ExecutorService created per-request, never shut down
 *
 * Run:   java -Xmx64m -XX:+HeapDumpOnOutOfMemoryError -XX:HeapDumpPath=heap.hprof \
 *             06_ExecutorLeak.java
 *
 * OOM:   java.lang.OutOfMemoryError: Java heap space
 *        (or: java.lang.OutOfMemoryError: unable to create new native thread
 *         if the thread count limit is hit first)
 *
 * Root:  A new Executor is created for each request but never shut down. The
 *        ThreadPoolExecutor keeps its worker threads alive (they block on the task
 *        queue). Each thread holds a GC root, which keeps the executor, its queue,
 *        and all submitted-but-completed task objects live indefinitely.
 *
 *        Real-world trigger: code that "creates a thread pool" inside a loop or
 *        per-request handler, often written by developers who know they should
 *        "use a thread pool" but create a new one each time instead of sharing one.
 *
 * Heap dump:
 *   hprof-analyzer heap report heap.hprof --section threads
 *   → Hundreds / thousands of worker threads, each retaining its executor.
 *   hprof-analyzer heap query heap.hprof \
 *       --oql "SELECT COUNT(*) FROM java.util.concurrent.ThreadPoolExecutor"
 *
 * Fix:   Create ONE shared executor at startup and inject / reuse it:
 *          static final ExecutorService SHARED = Executors.newFixedThreadPool(
 *              Runtime.getRuntime().availableProcessors());
 *        Shut it down only on application shutdown:
 *          Runtime.getRuntime().addShutdownHook(
 *              new Thread(() -> SHARED.shutdownNow()));
 */
import java.util.*;
import java.util.concurrent.*;

public class _06_ExecutorLeak {

    static final List<ExecutorService> LEAKED_EXECUTORS = new ArrayList<>();

    static void handleRequest(int id) throws Exception {
        // ❌ BAD: new executor per request, never shut down
        ExecutorService executor = Executors.newFixedThreadPool(2);
        LEAKED_EXECUTORS.add(executor); // makes leak visible; in real code executor
                                        // might just be a local var that goes out of scope
                                        // while threads keep it alive via GC roots

        executor.submit(() -> computeResult(id)).get(); // wait for result

        // ✅ FIX: call executor.shutdown() / executor.shutdownNow() here
        //         Or better: use a shared static executor (see class comment above)
    }

    static int computeResult(int id) { return id * 2; }

    public static void main(String[] args) throws Exception {
        System.out.println("Creating an ExecutorService per request without shutdown … (OOM expected)");
        for (int i = 0; ; i++) {
            handleRequest(i);
            if (i % 500 == 0) {
                Runtime rt = Runtime.getRuntime();
                System.out.printf("  requests=%,5d  leaked-executors=%,5d  threads≈%,5d  heap=%,d MB%n",
                    i,
                    LEAKED_EXECUTORS.size(),
                    LEAKED_EXECUTORS.size() * 2, // 2 threads per executor
                    (rt.totalMemory() - rt.freeMemory()) >> 20);
            }
        }
    }
}

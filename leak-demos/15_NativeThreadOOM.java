/**
 * SCENARIO 15 — Unable to create new native thread
 *
 * Run:   java 15_NativeThreadOOM.java
 *        (No -Xmx needed — this is an OS-level limit, not a heap limit)
 *
 * OOM:   java.lang.OutOfMemoryError: unable to create new native thread
 *
 * Root:  Each Java thread maps to one OS (kernel) thread and reserves a native
 *        stack segment (default: 256 KB–1 MB depending on OS and JVM flags).
 *        The JVM cannot create a thread when:
 *          (a) The process hits the OS thread-count limit (ulimit -u on Linux, typically
 *              ~32 768 per user or 4096 per process)
 *          (b) The process virtual address space is exhausted by stack reservations
 *              (relevant on 32-bit JVMs or very low -Xss)
 *          (c) The kernel refuses to create more threads (out of PIDs / task_max)
 *
 *        Common real-world cause: an ExecutorService, Netty channel, or reactive
 *        framework creates one thread per connection / request without bounding the
 *        pool size. Under load, threads accumulate faster than tasks complete.
 *
 * Observation:
 *   watch -n1 "ps -T -p $(pgrep -f NativeThreadOOM) | wc -l"  # live thread count
 *   jcmd <pid> Thread.print | wc -l                            # JVM thread list
 *
 * Heap dump:
 *   hprof-analyzer heap report heap.hprof --section threads
 *   → Thousands of threads, each retaining their stack variables.
 *   hprof-analyzer heap query heap.hprof \
 *       --oql "SELECT COUNT(*) FROM java.lang.Thread"
 *
 * Fix:   Always bound thread pools: Executors.newFixedThreadPool(N) or
 *        new ThreadPoolExecutor(core, max, ..., new ArrayBlockingQueue<>(capacity)).
 *        Prefer virtual threads (Java 21+) for I/O-bound workloads:
 *          Executors.newVirtualThreadPerTaskExecutor()
 *        Virtual threads are cheap (heap-allocated, not OS threads) and do not
 *        cause this OOM even at millions of concurrent tasks.
 */
public class _15_NativeThreadOOM {

    public static void main(String[] args) throws Exception {
        System.out.println("Creating threads without limit … (native thread OOM expected)");
        System.out.println("  This may take a while and will stress the OS — Ctrl+C to abort early.");

        long count = 0;
        while (true) {
            Thread t = new Thread(() -> {
                try { Thread.sleep(Long.MAX_VALUE); }
                catch (InterruptedException ignored) {}
            });
            t.setDaemon(true); // daemon so JVM can exit after OOM
            t.start();
            count++;

            if (count % 500 == 0)
                System.out.printf("  threads started: %,d%n", count);
        }
    }
}

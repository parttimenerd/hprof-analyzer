/**
 * WASTE 38 — One-thread-per-connection antipattern (500 sleeping threads)
 *
 * A naive "server" creates one thread per connection without pooling.
 * Each thread holds a ConnectionState (~8 KB) for as long as the connection
 * lives. At dump time: 500 threads sleeping, collectively retaining ~4 MB
 * of connection state.
 *
 * Run:   java -Xmx256m 38_ExcessiveThreads.java
 *
 * Capture (after "HEAP_DUMP_READY" is printed):
 *   jcmd $(pgrep -f ExcessiveThreads) GC.heap_dump heap.hprof
 *
 * What's in the heap:
 *   500 connection-handler threads, each retaining 1 ConnectionState
 *   ConnectionState: sessionId (String), remoteAddr (String),
 *                    requestBuffer (byte[8192]), headers (Map<String,String>)
 *
 * Root cause:
 *   Each new connection spawns a dedicated thread. Threads are GC roots —
 *   everything reachable from a thread's stack (and fields of Runnable) is
 *   retained until the thread terminates. With 500 long-lived connections
 *   and no upper bound on thread count, memory grows linearly.
 *
 * Real-world signs:
 *   - Thread count in jcmd Thread.print or via jstack grows without bound
 *   - Heap dominated by Thread → ConnectionState (or similar per-connection object)
 *   - DominantGcRootType triage fires: "100% of heap held by Thread roots"
 *
 * Fix:
 *   Replace one-thread-per-connection with a thread pool (Executors.newFixedThreadPool)
 *   or non-blocking I/O (java.nio, Netty, Vertx). Bound the maximum concurrency.
 *
 * Analysis:
 *   hprof-analyzer heap report heap.hprof
 *   → "gc-root-type" Warning: Thread roots dominate
 *   → Biggest classes will show _38_ExcessiveThreads$ConnectionState
 *
 *   hprof-analyzer heap query heap.hprof \
 *       --oql "SELECT COUNT(*) FROM _38_ExcessiveThreads\$ConnectionState"
 *   → 500
 */
import java.util.*;

public class _38_ExcessiveThreads {

    static class ConnectionState {
        final String              sessionId;
        final String              remoteAddr;
        final byte[]              requestBuffer;  // 8 KB per connection
        final Map<String, String> headers;

        ConnectionState(int connId) {
            this.sessionId     = "sess-" + connId + "-" + Long.toHexString(connId * 0x9E3779B97L);
            this.remoteAddr    = "10." + (connId / 256 % 256) + "." + (connId % 256) + ":4" + (connId % 10000 + 10000);
            this.requestBuffer = new byte[8192];
            this.headers       = new LinkedHashMap<>();

            // Simulate 10 realistic HTTP-style headers
            headers.put("Host",              "example.com");
            headers.put("User-Agent",        "Mozilla/5.0 (conn=" + connId + ")");
            headers.put("Accept",            "text/html,application/json");
            headers.put("Accept-Language",   "en-US,en;q=0.9");
            headers.put("Accept-Encoding",   "gzip, deflate, br");
            headers.put("Connection",        "keep-alive");
            headers.put("Cookie",            "session=" + sessionId);
            headers.put("X-Request-Id",      "req-" + connId);
            headers.put("X-Forwarded-For",   remoteAddr.split(":")[0]);
            headers.put("X-Connection-Time", String.valueOf(System.currentTimeMillis()));
        }
    }

    public static void main(String[] args) throws Exception {
        int connectionCount = 500;

        System.out.println("=== One-Thread-Per-Connection Server (naive) ===");
        System.out.printf("Spawning %d connection-handler threads …%n", connectionCount);

        List<Thread> threads = new ArrayList<>(connectionCount);
        for (int i = 0; i < connectionCount; i++) {
            final ConnectionState state = new ConnectionState(i);
            Thread t = new Thread(() -> {
                // Simulate a long-lived connection: hold state and sleep
                try {
                    Thread.sleep(Long.MAX_VALUE);
                } catch (InterruptedException ignored) {
                    // connection closed
                }
                // state is referenced here to keep it alive on the thread
                if (state.requestBuffer.length < 0) throw new AssertionError();
            });
            t.setName("connection-handler-" + i);
            t.setDaemon(true);
            t.start();
            threads.add(t);
        }

        // Give all threads a moment to start
        Thread.sleep(500);

        Runtime rt = Runtime.getRuntime();
        System.gc();
        Thread.sleep(200);

        long usedKB = (rt.totalMemory() - rt.freeMemory()) >> 10;
        System.out.printf("Heap used: %,d KB / %,d KB%n", usedKB, rt.maxMemory() >> 10);
        System.out.printf("Active connection threads: %d%n", threads.size());
        System.out.printf("Estimated connection state: ~%,d KB%n",
            (long) connectionCount * (8192 + 512) >> 10);
        System.out.println();
        System.out.println("HEAP_DUMP_READY");
        System.out.println("  jcmd $(pgrep -f ExcessiveThreads) GC.heap_dump heap.hprof");
        Thread.sleep(Long.MAX_VALUE);
    }
}

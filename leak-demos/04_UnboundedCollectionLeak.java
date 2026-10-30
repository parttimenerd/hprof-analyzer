/**
 * SCENARIO 04 — Unbounded collection: queue / log / deque growing without eviction
 *
 * Run:   java -Xmx64m -XX:+HeapDumpOnOutOfMemoryError -XX:HeapDumpPath=heap.hprof \
 *             04_UnboundedCollectionLeak.java
 *
 * OOM:   java.lang.OutOfMemoryError: Java heap space
 *
 * Root:  EVENT_LOG is an ArrayList that records every event but is never trimmed.
 *        This is structurally identical to the static-cache leak (scenario 01) but
 *        represents a different intent: an audit log, metrics buffer, request queue,
 *        or undo history that was designed to grow but lacks a size cap or TTL.
 *
 *        Also shown: the "stale element in array" sub-pattern — removing an element
 *        from an array-backed structure (like a Stack or manual ArrayList) without
 *        nulling the slot keeps the object alive past its logical lifetime
 *        (the "loitering object" bug described in Effective Java item 7).
 *
 * Heap dump:
 *   hprof-analyzer heap report heap.hprof --section leaks
 *   → Suspect: EVENT_LOG → ArrayList → Object[] → LogEntry instances
 *   hprof-analyzer heap query heap.hprof \
 *       --oql "SELECT l, l.size FROM java.util.ArrayList l ORDER BY l.size DESC LIMIT 5"
 *
 * Fix:   (a) Use a bounded deque: new ArrayDeque<>() + poll when over capacity.
 *        (b) Use a ring buffer (e.g., ArrayBlockingQueue with fixed capacity).
 *        (c) Apply TTL: evict entries older than N seconds.
 *        (d) For the stale-element pattern: null out array slots after logical removal.
 */
import java.util.*;

public class _04_UnboundedCollectionLeak {

    record LogEntry(long timestamp, String message, byte[] payload) {}

    // ❌ BAD: grows without bound — every event ever produced stays in memory
    static final List<LogEntry> EVENT_LOG = new ArrayList<>();

    static void recordEvent(int seq) {
        EVENT_LOG.add(new LogEntry(
            System.currentTimeMillis(),
            "event-" + seq,
            new byte[1_000] // 1 KB of "event data"
        ));
        // ✅ FIX: cap the log — keep only the last N entries:
        // if (EVENT_LOG.size() > 10_000) EVENT_LOG.remove(0);
        // Better: use ArrayDeque and poll() from the head
    }

    // ---- Bonus: stale element in an array-backed stack ---------------------
    static final Object[] MANUAL_STACK = new Object[1_000_000];
    static int stackTop = 0;

    static void push(Object o) { MANUAL_STACK[stackTop++] = o; }

    static Object pop() {
        Object val = MANUAL_STACK[--stackTop];
        // ❌ BAD: MANUAL_STACK[stackTop] still holds the reference → loitering object
        // ✅ FIX: MANUAL_STACK[stackTop] = null;  // null out the slot
        return val;
    }

    public static void main(String[] args) {
        System.out.println("Filling unbounded event log … (OOM expected)");
        for (int i = 0; ; i++) {
            recordEvent(i);
            if (i % 10_000 == 0) {
                Runtime rt = Runtime.getRuntime();
                System.out.printf("  log-size=%,8d  heap=%,d / %,d MB%n",
                    EVENT_LOG.size(),
                    (rt.totalMemory() - rt.freeMemory()) >> 20,
                    rt.maxMemory() >> 20);
            }
        }
    }
}

/**
 * SCENARIO 02 — Event listeners registered but never unregistered
 *
 * Run:   java -Xmx64m -XX:+HeapDumpOnOutOfMemoryError -XX:HeapDumpPath=heap.hprof \
 *             02_ListenerLeak.java
 *
 * OOM:   java.lang.OutOfMemoryError: Java heap space
 *
 * Root:  The EventBus holds a strong reference to every registered listener via
 *        its listeners list. Each listener lambda captures a large byte[] in its
 *        closure. Because nothing ever calls bus.unsubscribe(), the list grows
 *        indefinitely and all captured byte[] arrays are retained.
 *
 *        The bug is structural: the *publisher* (EventBus) outlives the *subscriber*
 *        (RequestHandler), but there is no lifecycle callback to clean up.
 *
 * Heap dump:
 *   hprof-analyzer heap report heap.hprof --section leaks
 *   → Suspect: EventBus → listeners (ArrayList) → lambda instances → byte[]
 *   hprof-analyzer heap query heap.hprof \
 *       --oql "SELECT COUNT(*) FROM java.util.ArrayList"
 *
 * Fix:   (a) Call bus.unsubscribe(listener) when the subscriber is no longer needed.
 *        (b) Use a WeakReference list so listeners can be GC'd without explicit removal
 *            (but this requires care — GC may collect them unexpectedly).
 *        (c) Use a scoped event bus tied to a lifecycle (e.g., one bus per request).
 */
import java.util.*;
import java.util.function.*;

public class _02_ListenerLeak {

    static class EventBus {
        private final List<Consumer<String>> listeners = new ArrayList<>();

        void subscribe(Consumer<String> listener) {
            listeners.add(listener); // ❌ no corresponding unsubscribe
        }

        void unsubscribe(Consumer<String> listener) {
            listeners.remove(listener); // ✅ FIX: call this when listener is done
        }

        void fire(String event) {
            listeners.forEach(l -> l.accept(event));
        }
    }

    static final EventBus BUS = new EventBus();

    static void simulateRequest(int id) {
        byte[] context = new byte[5_000]; // 5 KB "request context" captured by lambda
        BUS.subscribe(event -> {
            if (id % 50_000 == 0) // prevent dead-code elimination
                System.out.println("Handling " + event + " with context len=" + context.length);
        });
        // ❌ BAD: listener registered but never unsubscribed
        // ✅ FIX: save the lambda reference and call BUS.unsubscribe(listener) here
    }

    public static void main(String[] args) {
        System.out.println("Registering listeners without unsubscribing … (OOM expected)");
        for (int i = 0; ; i++) {
            simulateRequest(i);
            if (i % 10_000 == 0) {
                Runtime rt = Runtime.getRuntime();
                System.out.printf("  requests=%,8d  heap=%,d / %,d MB%n",
                    i,
                    (rt.totalMemory() - rt.freeMemory()) >> 20,
                    rt.maxMemory() >> 20);
            }
        }
    }
}

package com.example.leakclinic;

import org.springframework.stereotype.Component;
import java.util.ArrayList;
import java.util.List;

/**
 * LEAK 3 — ThreadLocal never cleared in a thread pool.
 *
 * Each request appends context strings to a ThreadLocal list. Because Spring
 * uses a thread pool, the thread is returned (not destroyed), so the list
 * survives and grows with every subsequent request handled by the same thread.
 *
 * What you'll find in the hprof-analyzer report:
 *   • Threads section → each HTTP worker thread retains a large ArrayList
 *   • Leak Suspects  → ThreadLocalMap entries retaining accumulated data
 *
 * This pattern is also a security concern: request data from user A bleeds
 * into the thread that later serves user B.
 */
@Component
public class RequestTracker {

    // BUG: never calls .remove() — data accumulates across requests on the same thread
    private static final ThreadLocal<List<String>> CONTEXT =
            ThreadLocal.withInitial(ArrayList::new);

    /** Simulate recording request context for "distributed tracing". */
    public void track(String requestId) {
        List<String> ctx = CONTEXT.get();
        // Each entry is ~500 chars of "trace metadata"
        ctx.add("trace-id=" + requestId
                + " span=" + Thread.currentThread().getName()
                + " timestamp=" + System.nanoTime()
                + " baggage=" + "x".repeat(400));
        // Missing: CONTEXT.remove() in a finally block!
    }

    /** Returns the number of accumulated entries on the current thread. */
    public int currentThreadEntries() {
        return CONTEXT.get().size();
    }
}

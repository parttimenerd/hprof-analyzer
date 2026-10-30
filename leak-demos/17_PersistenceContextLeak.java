/**
 * SCENARIO 17 — Persistence-context (Hibernate first-level cache) growing unbounded
 *
 * Run:   java -Xmx64m -XX:+HeapDumpOnOutOfMemoryError -XX:HeapDumpPath=heap.hprof \
 *             17_PersistenceContextLeak.java
 *
 * OOM:   java.lang.OutOfMemoryError: Java heap space
 *
 * Root:  Hibernate's EntityManager / Session keeps every loaded entity in its
 *        first-level cache (the "persistence context") for the duration of the
 *        transaction. When you stream or iterate through large result sets within
 *        a single long-running transaction WITHOUT calling session.clear()
 *        periodically, every entity stays pinned in memory for the whole loop.
 *
 *        This demo uses a pure-Java simulation of the session cache so it runs
 *        without Hibernate on the classpath.
 *
 * Heap dump:
 *   hprof-analyzer heap report heap.hprof --section leaks
 *   → Top suspect: FakeSession  (retains all Entity instances)
 *   hprof-analyzer heap query heap.hprof \
 *       --oql "SELECT COUNT(*) FROM _17_PersistenceContextLeak$Entity"
 *
 * Fix (Hibernate):
 *   (a) Batch-process with periodic session.clear() + session.flush():
 *         if (i % BATCH_SIZE == 0) { session.flush(); session.clear(); }
 *   (b) Use a StatelessSession for read-only bulk operations — no first-level cache.
 *   (c) Use scroll() / stream() with FORWARD_ONLY + READ_ONLY + fetch size:
 *         session.createQuery("FROM Entity", Entity.class)
 *                .setReadOnly(true).setFetchSize(100).getResultStream()
 *
 * Spring Data:
 *   @Transactional + repository.findAll() with a large table → same OOM.
 *   Fix: use repository.findAllBy...() with Pageable or a @QueryHints with
 *        HINT_FETCH_SIZE + stream().
 */
import java.util.*;

public class _17_PersistenceContextLeak {

    // ---- Simulated Hibernate Entity ----------------------------------------
    static class Entity {
        final int id;
        final byte[] payload = new byte[2_000]; // 2 KB "lazy-loaded columns"
        Entity(int id) { this.id = id; }
    }

    // ---- Simulated Session (first-level cache = identity map) ---------------
    static class FakeSession {
        // Hibernate keeps entities in an identity map keyed by (class, id)
        final Map<Integer, Entity> firstLevelCache = new HashMap<>();

        Entity load(int id) {
            return firstLevelCache.computeIfAbsent(id, Entity::new);
        }

        void clear() {
            firstLevelCache.clear(); // ← the fix: call every BATCH_SIZE iterations
        }

        int cacheSize() { return firstLevelCache.size(); }
    }

    // ---- Simulated bulk export job ------------------------------------------
    static void exportAll(FakeSession session, int totalRows, boolean withClear) {
        final int BATCH_SIZE = 100;
        for (int i = 0; i < totalRows; i++) {
            Entity e = session.load(i);
            processEntity(e); // use entity (write to file, transform, etc.)

            if (withClear && i % BATCH_SIZE == 0) {
                session.clear(); // ✅ FIX: flush processed entities out of the cache
            }
        }
    }

    static void processEntity(Entity e) {
        // Simulate work (write to output stream, etc.)
        if (e.id % 50_000 == 0) {
            System.out.printf("  processed id=%,d  cache-size=%,d  used=%,d MB%n",
                e.id, 0, // cache size printed in main
                (Runtime.getRuntime().totalMemory() - Runtime.getRuntime().freeMemory()) >> 20);
        }
    }

    public static void main(String[] args) {
        FakeSession session = new FakeSession();
        System.out.println("Running bulk export WITHOUT session.clear() … (OOM expected)");
        System.out.println("  Change withClear=true to see the fix work.");

        // ❌ BAD: withClear=false → session cache holds ALL entities until OOM
        // ✅ FIX: withClear=true  → clear every 100 rows, memory stays flat
        boolean withClear = false;

        // Use a separate thread to print cache size while running
        FakeSession[] ref = {session};
        Thread monitor = new Thread(() -> {
            while (!Thread.currentThread().isInterrupted()) {
                try {
                    Thread.sleep(1_000);
                    System.out.printf("  [monitor] cache=%,d entities  used=%,d MB%n",
                        ref[0].cacheSize(),
                        (Runtime.getRuntime().totalMemory() - Runtime.getRuntime().freeMemory()) >> 20);
                } catch (InterruptedException e) { break; }
            }
        });
        monitor.setDaemon(true);
        monitor.start();

        exportAll(session, 10_000_000, withClear);
        monitor.interrupt();
        System.out.println("Export complete (only reached with withClear=true).");
    }
}

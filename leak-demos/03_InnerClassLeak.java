/**
 * SCENARIO 03 — Non-static inner class / anonymous class retaining outer instance
 *
 * Run:   java -Xmx64m -XX:+HeapDumpOnOutOfMemoryError -XX:HeapDumpPath=heap.hprof \
 *             03_InnerClassLeak.java
 *
 * OOM:   java.lang.OutOfMemoryError: Java heap space
 *
 * Root:  In Java, a non-static inner class (or anonymous class) implicitly holds
 *        a synthetic reference to its enclosing instance (this$0). Even if the
 *        caller only stores the inner object — never the outer — the outer stays
 *        alive as long as the inner is reachable.
 *
 *        Here: TASKS holds Runnable lambdas. Each lambda was created inside a
 *        method of a BigOuter instance. The lambda captures `this` (BigOuter) via
 *        the synthetic reference. BigOuter.data (1 MB) can never be freed.
 *
 *        Real-world form: a Swing listener, Android Activity-bound callback, or
 *        a Callable submitted to an ExecutorService that references an enclosing
 *        service/controller with large state.
 *
 * Heap dump:
 *   hprof-analyzer heap report heap.hprof --section leaks
 *   → Suspect: TASKS (List) → lambda instances → BigOuter (via this$0) → byte[]
 *   hprof-analyzer heap query heap.hprof \
 *       --oql "SELECT COUNT(*), @retainedHeapSize FROM _03_InnerClassLeak$BigOuter"
 *
 * Fix:   (a) Make the inner class / lambda static — don't capture the outer reference.
 *        (b) Extract the needed data into a local variable BEFORE the lambda:
 *              final int needed = this.smallField;  // not `this`
 *              return () -> use(needed);
 *        (c) Use a static factory method that takes only the required parameters.
 */
import java.util.*;

public class _03_InnerClassLeak {

    static class BigOuter {
        final int id;
        final byte[] data = new byte[1_000_000]; // 1 MB of "service state"

        BigOuter(int id) { this.id = id; }

        // ❌ BAD: anonymous class implicitly captures BigOuter.this
        Runnable createTask() {
            return new Runnable() {
                @Override public void run() {
                    // Uses `data` indirectly via BigOuter.this — outer instance pinned!
                    System.out.println("task from outer " + id);
                }
            };
            // ✅ FIX: use a static nested class or extract only what's needed:
            // final int capturedId = this.id;
            // return () -> System.out.println("task from outer " + capturedId);
        }
    }

    // Only Runnables are stored — caller never holds a reference to BigOuter
    static final List<Runnable> TASKS = new ArrayList<>();

    public static void main(String[] args) {
        System.out.println("Accumulating Runnables that silently pin BigOuter instances … (OOM expected)");
        for (int i = 0; ; i++) {
            BigOuter outer = new BigOuter(i); // 1 MB allocated
            TASKS.add(outer.createTask());    // outer kept alive via Runnable.this$0!
            // outer local variable goes out of scope — but BigOuter is NOT freed

            if (i % 100 == 0) {
                Runtime rt = Runtime.getRuntime();
                System.out.printf("  tasks=%,5d  heap=%,d / %,d MB%n",
                    TASKS.size(),
                    (rt.totalMemory() - rt.freeMemory()) >> 20,
                    rt.maxMemory() >> 20);
            }
        }
    }
}

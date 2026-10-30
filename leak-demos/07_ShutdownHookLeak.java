/**
 * SCENARIO 07 — Shutdown hook capturing a large object graph (classloader retention)
 *
 * Run:   java -Xmx64m -XX:+HeapDumpOnOutOfMemoryError -XX:HeapDumpPath=heap.hprof \
 *             07_ShutdownHookLeak.java
 *
 * OOM:   java.lang.OutOfMemoryError: Java heap space
 *
 * Root:  Runtime.addShutdownHook() registers a Thread in the JVM's shutdown hook
 *        list. The hook's Thread object (and everything it references via its
 *        Runnable) is held by the JVM — it is a GC root — until the JVM exits.
 *
 *        If a library or framework registers a shutdown hook from within a
 *        web application (e.g., inside a Tomcat webapp), the hook's anonymous
 *        Thread subclass captures `this` of the class that registered it, which
 *        may be loaded by the WebappClassLoader. That classloader — plus all
 *        10 000+ classes it loaded — is then pinned in memory (Metaspace) until
 *        JVM exit, not just until webapp undeploy.
 *
 *        Real-world case: Twilio SDK (github.com/twilio/twilio-java issue #690)
 *        registered a hook with a lambda that captured an ExecutorService field,
 *        preventing classloader GC on Tomcat redeploy.
 *
 *        This demo shows the simpler heap-space variant: hooks accumulate large
 *        closures (each capturing a 500 KB byte[]) across repeated "re-registrations"
 *        (simulating a hot-deploy loop that re-initialises the same library).
 *
 * Heap dump:
 *   hprof-analyzer heap report heap.hprof --section leaks
 *   → Suspect: JVM shutdown-hook list → Thread[] → lambda instances → byte[]
 *   hprof-analyzer heap query heap.hprof \
 *       --oql "SELECT COUNT(*) FROM java.lang.Thread WHERE @displayName LIKE 'shutdown*'"
 *
 * Fix:   (a) Track the registered hook and remove it on undeploy:
 *              Runtime.getRuntime().removeShutdownHook(hook);
 *        (b) In libraries: expose a close() / destroy() method instead of using hooks.
 *        (c) Never capture `this` from the enclosing class; use a static/side-effect-free
 *            lambda that only calls static cleanup methods.
 */
public class _07_ShutdownHookLeak {

    static volatile long hookCount = 0;

    static void initializeLibrary(int version) {
        byte[] config = new byte[500_000]; // 500 KB "config / state" captured by the hook

        Thread hook = new Thread(() -> {
            // ❌ BAD: anonymous Runnable (or lambda) captures `config` from enclosing scope
            System.out.println("shutting down library v" + version + " cfg=" + config.length);
        }, "shutdown-hook-v" + version);

        Runtime.getRuntime().addShutdownHook(hook); // hook (and config) pinned until JVM exit
        hookCount++;

        // ✅ FIX:
        // (a) Keep a reference to `hook` and call removeShutdownHook(hook) when done.
        // (b) Don't capture large objects; store config in a static field that can be nulled.
    }

    public static void main(String[] args) throws Exception {
        System.out.println("Re-registering shutdown hooks without removing old ones … (OOM expected)");
        System.out.println("  (Simulates a library being re-initialised on each hot-reload)");
        for (int i = 0; ; i++) {
            initializeLibrary(i); // each call leaks 500 KB via the hook
            if (i % 100 == 0) {
                Runtime rt = Runtime.getRuntime();
                System.out.printf("  hooks=%,5d  heap=%,d / %,d MB%n",
                    hookCount,
                    (rt.totalMemory() - rt.freeMemory()) >> 20,
                    rt.maxMemory() >> 20);
            }
        }
    }
}

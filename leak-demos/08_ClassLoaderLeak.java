/**
 * SCENARIO 08 — ClassLoader instances held, preventing class unloading (→ Metaspace OOM)
 *
 * Run:   java -XX:MaxMetaspaceSize=64m \
 *             -XX:+HeapDumpOnOutOfMemoryError -XX:HeapDumpPath=heap.hprof \
 *             08_ClassLoaderLeak.java
 *
 * OOM:   java.lang.OutOfMemoryError: Metaspace
 *
 * Root:  A class can only be unloaded when its ClassLoader becomes unreachable.
 *        Here, LOADER_SINK holds a strong reference to every ClassLoader ever created.
 *        Each ClassLoader loads a proxy class (defined inside that loader), which
 *        occupies Metaspace. Neither the loaders nor their classes are ever unloaded.
 *
 *        Real-world form (servlet containers, OSGi):
 *          A global singleton, thread, static field, JDBC driver, or shutdown hook
 *          keeps a reference to a webapp's WebappClassLoader. On redeploy, the old
 *          ClassLoader — plus ALL its loaded classes and their metadata — remains in
 *          Metaspace. Repeat 10 times → Metaspace exhaustion.
 *
 * Metaspace observation:
 *   # Watch Metaspace grow live:
 *   jcmd <pid> VM.native_memory summary
 *   # Or add JVM flags:
 *   -verbose:class -Xlog:class+unload=info
 *
 * Heap dump:
 *   hprof-analyzer heap histogram heap.hprof
 *   → Large count of java.lang.ClassLoader subclasses
 *   hprof-analyzer heap query heap.hprof \
 *       --oql "SELECT COUNT(*) FROM java.net.URLClassLoader"
 *
 *   Note: Metaspace is NOT in the Java heap, so the heap dump is small.
 *         Use `jcmd <pid> VM.native_memory` or NMT for native memory breakdown.
 *
 * Fix:   Never store ClassLoader instances in long-lived structures.
 *        In containers, implement ServletContextListener.contextDestroyed() to
 *        deregister JDBC drivers, cancel timers, and stop any threads that were
 *        started by the webapp, allowing the WebappClassLoader to be collected.
 *        Library: mjiderhamn/classloader-leak-prevention (Maven Central)
 */
import java.lang.reflect.*;
import java.net.*;
import java.util.*;

public class _08_ClassLoaderLeak {

    // ❌ BAD: every ClassLoader ever created is kept alive here
    static final List<ClassLoader> LOADER_SINK = new ArrayList<>();

    static ClassLoader createLeakyLoader() throws Exception {
        // Empty URL set; delegates to parent for all non-proxy loading.
        // Proxy.newProxyInstance defines the generated $Proxy class *inside* this loader
        // → the class lives in Metaspace for as long as the loader is alive.
        URLClassLoader cl = new URLClassLoader(
            new URL[0],
            _08_ClassLoaderLeak.class.getClassLoader()
        );

        // Force a class to be defined in `cl` (the proxy class counts)
        Proxy.newProxyInstance(cl,
            new Class[]{ Runnable.class, Cloneable.class },
            (proxy, method, args) -> null);

        return cl;
    }

    public static void main(String[] args) throws Exception {
        System.out.println("Creating ClassLoaders without releasing them … (Metaspace OOM expected)");
        for (int i = 0; ; i++) {
            ClassLoader cl = createLeakyLoader();
            LOADER_SINK.add(cl); // ← prevents GC → Metaspace grows

            // ✅ FIX: don't add to LOADER_SINK — let `cl` go out of scope
            //         The proxy class and ClassLoader become unreachable → GC unloads them

            if (i % 5_000 == 0) {
                System.out.printf("  loaders=%,8d%n", LOADER_SINK.size());
            }
        }
    }
}

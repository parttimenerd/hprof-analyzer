/**
 * SCENARIO 09 — Dynamic class generation filling Metaspace / Compressed Class Space
 *
 * Run:   java -XX:MaxMetaspaceSize=64m \
 *             -XX:+HeapDumpOnOutOfMemoryError -XX:HeapDumpPath=heap.hprof \
 *             09_ClassGenerationOOM.java
 *
 * OOM:   java.lang.OutOfMemoryError: Metaspace
 *        (or "Compressed class space" if UseCompressedClassPointers is on, which is
 *         the default on 64-bit JVMs; the 32-bit compressed space overflows first)
 *
 * Root:  Every call to Proxy.newProxyInstance with a distinct ClassLoader generates
 *        a new proxy class ($Proxy0, $Proxy1, …) and caches it per loader.
 *        When the loaders are not held strongly, the GC can unload them — but if
 *        class generation outpaces GC (e.g., at startup or in a hot path), Metaspace
 *        fills before GC can intervene.
 *
 *        Real-world triggers:
 *          - Spring AOP / CGLIB generating a subclass proxy per bean
 *          - Mockito generating a mock class per test
 *          - Groovy/Kotlin closures compiled to anonymous classes
 *          - Reflection-based serialization frameworks (Jackson, Kryo)
 *          - ASM or ByteBuddy code generation without class caching
 *
 * Fix:   Cache generated proxy/class per interface set + classloader.
 *        In Spring: enable cglib-proxy caching (it's on by default; disable via
 *        spring.aop.proxy-target-class=false if you see this in tests).
 *        For Metaspace tuning: -XX:MetaspaceSize=256m -XX:MaxMetaspaceSize=512m
 *        To disable compressed class space: -XX:-UseCompressedClassPointers (Java ≤24)
 *        Note: UseCompressedClassPointers is deprecated since Java 25.
 *
 * Heap dump:
 *   hprof-analyzer heap histogram heap.hprof
 *   → Many java.lang.Class instances, each retaining small but non-zero heap
 *   # Metaspace is off-heap; use NMT or -Xlog:class+load for class counts:
 *   java ... -Xlog:class+load=info:stdout 09_ClassGenerationOOM.java 2>&1 | wc -l
 */
import java.lang.reflect.*;
import java.net.*;
import java.util.*;

public class _09_ClassGenerationOOM {

    // The proxy class cache is keyed per ClassLoader.
    // A fresh (but immediately-GC-eligible) loader per iteration generates a new class
    // faster than GC can unload old ones at high rate.
    static Object generateProxy() throws Exception {
        // Each new URLClassLoader gives Proxy a fresh namespace → new class generated
        ClassLoader cl = new URLClassLoader(
            new URL[0],
            _09_ClassGenerationOOM.class.getClassLoader()
        );
        return Proxy.newProxyInstance(
            cl,
            new Class[]{ Runnable.class },
            (proxy, method, args) -> null
        );
        // `cl` and proxy go out of scope here → eligible for GC …
        // … but Metaspace allocation outpaces GC at high rates
    }

    public static void main(String[] args) throws Exception {
        System.out.println("Generating proxy classes rapidly … (Metaspace OOM expected)");
        for (int i = 0; ; i++) {
            generateProxy();
            if (i % 10_000 == 0)
                System.out.printf("  classes generated ≥ %,d%n", i);
        }
    }
}

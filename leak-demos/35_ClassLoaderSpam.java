/**
 * WASTE 35 — ClassLoader spam without a persistent reference
 *
 * Shows many Class/ClassLoader objects at dump time even without a real leak,
 * because GC hasn't fully reclaimed them yet. Useful for exploring the class
 * histogram and seeing what "transient ClassLoader pressure" looks like on
 * the heap — contrast with scenario 36, which is a true persistent leak.
 *
 * Run:   java -Xmx128m 35_ClassLoaderSpam.java
 *
 * Capture (after "HEAP_DUMP_READY" is printed):
 *   jcmd $(pgrep -f ClassLoaderSpam) GC.heap_dump heap.hprof
 *
 * What's in the heap at dump time:
 *   500 URLClassLoader instances                → created but NOT held strongly
 *   500 proxy Class objects ($Proxy0…)          → defined inside each loader
 *   URLClassLoader / Proxy internal bookkeeping → sun.misc.ProxyGenerator data
 *   (GC may have reclaimed some by dump time — that's intentional)
 *
 * Interesting analysis:
 *   hprof-analyzer heap histogram heap.hprof
 *   → look for java.net.URLClassLoader, $Proxy classes, sun.reflect.*
 *
 *   hprof-analyzer heap query heap.hprof \
 *       --oql "SELECT COUNT(*) FROM java.net.URLClassLoader"
 *   → may be 0 if GC ran; re-run without explicit System.gc() for more noise
 *
 *   hprof-analyzer heap report heap.hprof --section triage
 *   → should show NO dominant retainer (healthy/diffuse heap shape)
 *
 * Fix:   Nothing to fix — this is normal transient pressure.
 *        If URLClassLoader count stays high across multiple GC cycles,
 *        you have a real leak (see scenario 36).
 */
import java.lang.reflect.*;
import java.net.*;

public class _35_ClassLoaderSpam {

    static final int LOADER_COUNT = 500;

    public static void main(String[] args) throws Exception {
        System.out.println("=== ClassLoader Spam (no leak) ===");
        System.out.printf("Creating %,d URLClassLoaders (no strong references kept) …%n", LOADER_COUNT);

        for (int i = 0; i < LOADER_COUNT; i++) {
            // Create a URLClassLoader but do NOT store it anywhere persistent
            URLClassLoader cl = new URLClassLoader(
                new URL[0],
                _35_ClassLoaderSpam.class.getClassLoader()
            );
            // Define a proxy class inside this loader — forces a Class object
            // into the loader's namespace, visible in heap histogram
            Proxy.newProxyInstance(cl,
                new Class[]{ Runnable.class },
                (proxy, method, margs) -> null);
            // cl goes out of scope here — eligible for GC, but not yet collected
            if (i % 100 == 99) {
                System.out.printf("  created %,d loaders …%n", i + 1);
            }
        }

        // Suggest (but don't force) collection — snapshot is taken right after,
        // so some loaders/classes may still be on the heap
        System.gc();
        Thread.sleep(200);

        Runtime rt = Runtime.getRuntime();
        long used = rt.totalMemory() - rt.freeMemory();
        System.out.printf("Done | heap used %,d MB / max %,d MB%n",
            used >> 20, rt.maxMemory() >> 20);
        System.out.println();
        System.out.println("HEAP_DUMP_READY");
        System.out.println("  jcmd $(pgrep -f ClassLoaderSpam) GC.heap_dump heap.hprof");
        Thread.sleep(Long.MAX_VALUE);
    }
}

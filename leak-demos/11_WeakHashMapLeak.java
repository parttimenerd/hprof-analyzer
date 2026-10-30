/**
 * SCENARIO 11 — WeakHashMap where the value retains the key (entry never expires)
 *
 * Run:   java -Xmx64m -XX:+HeapDumpOnOutOfMemoryError -XX:HeapDumpPath=heap.hprof \
 *             11_WeakHashMapLeak.java
 *
 * Root:  WeakHashMap uses WeakReference<K> for its keys. An entry is removed when
 *        its key becomes *weakly reachable* — i.e., reachable only through weak
 *        references. But if the VALUE holds a strong reference back to the KEY,
 *        the key is always strongly reachable via:
 *          WeakHashMap → entry.value → Metadata.owner → key
 *        The key can never become weakly reachable, so the entry NEVER expires.
 *        The map grows without bound despite using "weak" keys.
 *
 *        This is a well-known Eclipse MAT inspection:
 *          "Reference Leak" (help.eclipse.org/latest, org.eclipse.mat.ui.help/
 *           reference/inspections/reference_leak.html)
 *
 * OOM:   java.lang.OutOfMemoryError: Java heap space
 *
 * Heap dump:
 *   hprof-analyzer heap report heap.hprof --section leaks
 *   → Suspect: WeakHashMap  (never shrinks)
 *   hprof-analyzer heap query heap.hprof \
 *       --oql "SELECT m, m.size FROM java.util.WeakHashMap m ORDER BY m.size DESC LIMIT 5"
 *
 *   # MAT's "Reference Leak" inspection also catches this pattern directly.
 *
 * Fix:   Values in a WeakHashMap must NOT strongly reference their key.
 *        If you need the key accessible from the value, store only a copy of the
 *        key's identity (e.g., System.identityHashCode, a name string, or an ID)
 *        rather than the object reference itself.
 */
import java.util.*;

public class _11_WeakHashMapLeak {

    static class Metadata {
        final Object owner; // ❌ BAD: strong reference back to the WeakHashMap key!
        final byte[] data = new byte[5_000]; // 5 KB of metadata

        Metadata(Object owner) { this.owner = owner; }
        // ✅ FIX: don't hold `owner` reference (or use only owner.toString() / an ID)
    }

    // Would work correctly if Metadata did NOT hold owner
    static final Map<Object, Metadata> REGISTRY = new WeakHashMap<>();

    static void register(Object component) {
        REGISTRY.put(component, new Metadata(component));
        // `component` local var goes out of scope after this call.
        // Expected: entry expires. Actual: Metadata.owner keeps component alive → never expires.
    }

    public static void main(String[] args) throws Exception {
        System.out.println("Registering components in WeakHashMap with back-referencing value … (OOM expected)");
        for (int i = 0; ; i++) {
            register(new Object()); // local variable — intended to become GC-eligible

            if (i % 5_000 == 0) {
                System.gc(); // hint to GC — won't help because of the strong cycle
                Runtime rt = Runtime.getRuntime();
                System.out.printf("  registered=%,8d  map.size=%,8d  used=%,d MB%n",
                    i, REGISTRY.size(),
                    (rt.totalMemory() - rt.freeMemory()) >> 20);
            }
        }
    }
}

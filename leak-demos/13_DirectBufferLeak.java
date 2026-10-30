/**
 * SCENARIO 13 — Direct (off-heap) ByteBuffer never released
 *
 * Run:   java -Xmx32m -XX:MaxDirectMemorySize=64m \
 *             -XX:+HeapDumpOnOutOfMemoryError -XX:HeapDumpPath=heap.hprof \
 *             13_DirectBufferLeak.java
 *
 * OOM:   java.lang.OutOfMemoryError: Direct buffer memory
 *
 * Root:  ByteBuffer.allocateDirect() allocates memory OUTSIDE the Java heap in
 *        native/off-heap memory. A sun.misc.Cleaner (or java.lang.ref.Cleaner in
 *        Java 9+) registered on the ByteBuffer object frees the native memory when
 *        the ByteBuffer becomes unreachable AND the GC runs.
 *
 *        Here we keep strong references to all DirectByteBuffers in a list.
 *        The Cleaner never fires because the buffers are always strongly reachable.
 *        The Java heap stays almost empty — which is why the heap dump is misleading:
 *        the heap dump looks fine while the process RSS (resident set size) grows.
 *
 *        Real-world trigger: NIO channels, Netty (ByteBuf backed by direct memory),
 *        SSL/TLS operations. Easy to miss because heap monitoring doesn't show it.
 *
 * Observation (no heap dump needed):
 *   watch -n1 "ps -o rss= -p $(pgrep -f DirectBufferLeak)"   # RSS grows despite tiny heap
 *   # Or: jcmd <pid> VM.native_memory summary | grep -A2 Internal
 *
 * Heap dump (small but shows the ByteBuffer objects):
 *   hprof-analyzer heap query heap.hprof \
 *       --oql "SELECT COUNT(*) FROM java.nio.DirectByteBuffer"
 *   # The heap looks fine — the actual leak is in native memory!
 *
 * Fix:   (a) Don't hold references to direct buffers longer than needed.
 *        (b) Use a ByteBuffer pool (e.g., Netty's PooledByteBufAllocator).
 *        (c) Explicitly free: ((sun.nio.ch.DirectBuffer) buf).cleaner().clean()  [Java ≤ 17]
 *            or use MemorySegment.ofBuffer(buf).unload()  [Java 22+ Foreign Memory API]
 *        (d) Set -XX:MaxDirectMemorySize to bound the exposure.
 */
import java.nio.*;
import java.util.*;

public class _13_DirectBufferLeak {

    static final List<ByteBuffer> BUFFERS = new ArrayList<>();

    public static void main(String[] args) throws Exception {
        System.out.println("Allocating direct buffers without releasing … (off-heap OOM expected)");
        System.out.println("  Note: Java heap will look nearly empty in the heap dump!");

        for (int i = 0; ; i++) {
            ByteBuffer buf = ByteBuffer.allocateDirect(1_024 * 1_024); // 1 MB off-heap
            BUFFERS.add(buf); // ❌ prevents Cleaner from running

            // ✅ FIX: don't retain the buffer — let it go out of scope
            //         The Cleaner fires when GC collects the ByteBuffer object

            if (i % 10 == 0) {
                Runtime rt = Runtime.getRuntime();
                System.out.printf("  buffers=%,4d  heap-used=%,d MB  (off-heap ≈ %,d MB)%n",
                    BUFFERS.size(),
                    (rt.totalMemory() - rt.freeMemory()) >> 20,
                    (long) BUFFERS.size()); // 1 MB per buffer
            }
        }
    }
}

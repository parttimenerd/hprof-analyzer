/**
 * LEAK 40 — DirectByteBuffer connection buffer pool leak (100 × 1 MB off-heap buffers)
 *
 * Run:   java -Xmx64m -XX:MaxDirectMemorySize=256m 40_DirectBufferLeak.java
 *
 * Capture (after "HEAP_DUMP_READY" is printed):
 *   jcmd $(pgrep -f DirectBufferLeak) GC.heap_dump heap.hprof
 *
 * Pattern:
 *   A NIO server allocates one DirectByteBuffer per accepted connection for
 *   zero-copy I/O. When the connection closes, the buffer is added to a
 *   "free pool" for reuse — but the pool grows without bound and never evicts.
 *   The DirectByteBuffer wrapper objects are on-heap (small), but each one
 *   pins a native off-heap memory region that the JVM cannot collect as long
 *   as the wrapper is reachable. Total off-heap usage: 100 × 1 MB = ~100 MB.
 *
 *   Unlike scenario 13 (DirectBufferLeak OOM), this scenario does NOT run to
 *   OOM — it allocates a fixed 100 buffers and then waits, giving the analyzer
 *   a clean snapshot of a live off-heap leak at steady state.
 *
 *   Real-world triggers:
 *     - Netty channel buffers allocated with ByteBufAllocator.directBuffer()
 *       not released via ReferenceCountUtil.release()
 *     - Java NIO servers that never call Cleaner.clean() on their read/write
 *       buffers after socket close
 *     - "Buffer pool" caches with no size cap and no eviction policy
 *     - MappedByteBuffer regions (memory-mapped files) held in a registry
 *
 * What's in the heap (Java heap — small):
 *   100 java.nio.DirectByteBuffer wrapper objects (~48 B each on-heap)
 *   100 Connection objects (id, buffer ref, metadata map, timestamp)
 *   100 sun.nio.ch.DirectBuffer.Deallocator / Cleaner objects (internal)
 *
 * What's off-heap (NOT in the Java heap dump, but real):
 *   ~100 MB of native direct memory, each region pinned by a wrapper object
 *
 * Interesting analysis:
 *   hprof-analyzer heap report heap.hprof --section triage
 *   → "Off-heap (DirectByteBuffer)" signal fires — shows total off-heap bytes
 *
 *   hprof-analyzer heap report heap.hprof --section indicators
 *   → DirectByteBuffer total reported (capacity sum, not heap size)
 *
 *   hprof-analyzer heap histogram heap.hprof
 *   → java.nio.DirectByteBuffer: 100 instances, small on-heap retained size
 *     (the ~100 MB is off-heap — invisible to histogram, but shown by the
 *     off-heap indicator)
 *
 *   hprof-analyzer heap query heap.hprof \
 *       --oql "SELECT @objectId AS idx, capacity AS cap \
 *              FROM java.nio.DirectByteBuffer ORDER BY cap DESC LIMIT 10"
 *
 * Fix:
 *   Call sun.misc.Cleaner.clean() (internal API) or, better:
 *     ((sun.nio.ch.DirectBuffer) buf).cleaner().clean()
 *   Or use try-with-resources wrappers, or switch to Netty's ref-counted buffers.
 *   In Java 9+, use java.lang.ref.Cleaner for explicit deallocation hooks.
 *   Always cap pooled-buffer collections and implement LRU eviction.
 */
import java.nio.ByteBuffer;
import java.util.*;

public class _40_DirectBufferLeak {

    static final int BUFFER_COUNT    = 100;
    static final int BUFFER_SIZE_MB  = 1;
    static final int BUFFER_BYTES    = BUFFER_SIZE_MB * 1024 * 1024;

    // ❌ BAD: "free pool" for reuse — grows without bound, never evicts
    static final List<Connection> BUFFER_SINK = new ArrayList<>();

    static class Connection {
        final String              id;
        final ByteBuffer          buffer;     // holds reference → pins off-heap region
        final Map<String, String> metadata;
        final long                createdAt;

        Connection(int idx) {
            this.id        = String.format("conn-%04d", idx);
            // ❌ allocateDirect → off-heap native memory, NOT on the Java heap
            // ✅ FIX: call ((sun.nio.ch.DirectBuffer) this.buffer).cleaner().clean()
            //         when the connection closes; or use a bounded, evicting pool
            this.buffer    = ByteBuffer.allocateDirect(BUFFER_BYTES);
            this.metadata  = new HashMap<>();
            this.metadata.put("remoteAddr", "10.0." + (idx / 256) + "." + (idx % 256));
            this.metadata.put("protocol",   "HTTP/1.1");
            this.metadata.put("keepAlive",  "true");
            this.metadata.put("bufferSize", String.valueOf(BUFFER_BYTES));
            this.metadata.put("pooledAt",   String.valueOf(System.currentTimeMillis()));
            this.createdAt = System.currentTimeMillis();

            // Simulate writing connection header into the buffer
            this.buffer.put((byte) 'H').put((byte) 'T').put((byte) 'T').put((byte) 'P');
            this.buffer.flip();
        }
    }

    public static void main(String[] args) throws Exception {
        System.out.println("=== DirectByteBuffer Connection Buffer Pool Leak ===");
        System.out.printf("Allocating %d × %d MB direct buffers (total ~%d MB off-heap) …%n",
            BUFFER_COUNT, BUFFER_SIZE_MB, (long) BUFFER_COUNT * BUFFER_SIZE_MB);

        for (int i = 0; i < BUFFER_COUNT; i++) {
            Connection conn = new Connection(i);
            BUFFER_SINK.add(conn); // ← the leak: buffer wrapper kept alive → off-heap pinned

            if ((i + 1) % 25 == 0) {
                System.out.printf("  allocated %3d buffers  (~%,d MB off-heap so far)%n",
                    i + 1, (long)(i + 1) * BUFFER_SIZE_MB);
            }
        }

        Runtime rt = Runtime.getRuntime();
        System.gc(); Thread.sleep(200);
        long usedHeapMB  = (rt.totalMemory() - rt.freeMemory()) >> 20;
        long offHeapMB   = (long) BUFFER_COUNT * BUFFER_SIZE_MB;

        System.out.printf("%nHeap:     %,d MB used / %,d MB max  (DirectByteBuffer wrappers are small on-heap)%n",
            usedHeapMB, rt.maxMemory() >> 20);
        System.out.printf("Off-heap: ~%,d MB pinned by %d DirectByteBuffer instances%n",
            offHeapMB, BUFFER_SINK.size());
        System.out.printf("  connections in pool: %,d%n", BUFFER_SINK.size());
        System.out.println();
        System.out.println("HEAP_DUMP_READY");
        System.out.println("  jcmd $(pgrep -f DirectBufferLeak) GC.heap_dump heap.hprof");
        Thread.sleep(Long.MAX_VALUE);
    }
}

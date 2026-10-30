/**
 * SCENARIO 22 — Duplicate byte[] arrays: many arrays with identical content
 *
 * Run:   java -Xmx128m 22_DuplicateByteArrayWaste.java
 *        (No OOM — WASTE scenario. Heap dump mid-run.)
 *
 * Capture:  jcmd $(pgrep -f DuplicateByteArrayWaste) GC.heap_dump heap.hprof
 *
 * Root:  Unlike String, byte[] has no interning mechanism. Every call to
 *        getBytes(), Base64.decode(), or Arrays.copyOf() produces a fresh array
 *        regardless of whether an identical array already exists.
 *
 *        Realistic sources:
 *        A) Per-request default HTTP headers stored as byte[] — identical for every
 *           request but allocated fresh each time and stashed in a response object.
 *        B) Cached icon / image data loaded for each user profile — same bytes,
 *           different array objects.
 *        C) Serialized "empty state" payloads (e.g., empty JSON "{}") cached per
 *           session separately.
 *        D) Encryption IV / salt buffers with a repeated default value.
 *
 * hprof-analyzer:
 *   hprof-analyzer heap.hprof report.html --find-duplicates
 *   → "Duplicate Primitive Arrays" section: byte[] top offenders
 *   hprof-analyzer heap query heap.hprof \
 *       --oql "SELECT @objectId, @length FROM byte[] WHERE @length > 100
 *               ORDER BY @length DESC LIMIT 20"
 *
 * Fix:
 *   (a) Use a single static final byte[] constant and share the reference.
 *   (b) For mutable buffers that must not be shared, clone only when needed.
 *   (c) For cache values, wrap in an immutable value type and deduplicate by content hash.
 */
import java.util.*;
import java.nio.charset.StandardCharsets;

public class _22_DuplicateByteArrayWaste {

    // ❌ BAD: returns a fresh byte[] every call — content is always identical
    static byte[] defaultResponseHeaders() {
        return ("Content-Type: application/json\r\n" +
                "Cache-Control: no-cache\r\n" +
                "X-Content-Type-Options: nosniff\r\n").getBytes(StandardCharsets.UTF_8);
    }

    // ✅ FIX: share one array (if immutable)
    static final byte[] DEFAULT_HEADERS =
        ("Content-Type: application/json\r\n" +
         "Cache-Control: no-cache\r\n" +
         "X-Content-Type-Options: nosniff\r\n").getBytes(StandardCharsets.UTF_8);

    static final byte[] DEFAULT_AVATAR = new byte[8192]; // 8 KB default avatar PNG
    static { Arrays.fill(DEFAULT_AVATAR, (byte) 0xFF); } // constant content

    // Simulates a per-request response object
    static class Response {
        final byte[] headers;  // should be a shared constant, but isn't
        final byte[] avatar;   // same for every user with no custom avatar
        final byte[] payload;  // genuinely unique per request

        Response(int requestId) {
            this.headers = defaultResponseHeaders();       // ❌ 118-byte copy per request
            this.avatar  = Arrays.copyOf(DEFAULT_AVATAR, DEFAULT_AVATAR.length); // ❌ 8KB copy per request
            this.payload = ("result-" + requestId).getBytes(StandardCharsets.UTF_8); // OK: unique
        }
    }

    public static void main(String[] args) throws Exception {
        int N = 50_000;
        System.out.println("Creating " + N + " Response objects with duplicate byte[] …");
        System.out.printf("  Expected duplicate headers: %d × %d bytes = %,.0f KB wasted%n",
            N, defaultResponseHeaders().length, N * defaultResponseHeaders().length / 1024.0);
        System.out.printf("  Expected duplicate avatars:  %d × %d bytes = %,.0f MB wasted%n",
            N, DEFAULT_AVATAR.length, N * DEFAULT_AVATAR.length / 1024.0 / 1024.0);

        List<Response> responses = new ArrayList<>(N);
        for (int i = 0; i < N; i++) {
            responses.add(new Response(i));
        }

        Runtime rt = Runtime.getRuntime();
        System.gc(); Thread.sleep(300);
        System.out.printf("%nHeap used: %,d MB / %,d MB%n",
            (rt.totalMemory() - rt.freeMemory()) >> 20, rt.maxMemory() >> 20);
        System.out.println();
        System.out.println("Take a heap dump now:");
        System.out.println("  jcmd $(pgrep -f DuplicateByteArrayWaste) GC.heap_dump heap.hprof");
        System.out.println("Then: hprof-analyzer heap.hprof report.html --find-duplicates");

        System.out.println(responses.get(0).headers.length); // prevent optimizer
        Thread.sleep(Long.MAX_VALUE);
    }
}

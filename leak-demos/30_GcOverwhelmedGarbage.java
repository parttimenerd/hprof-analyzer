/**
 * SCENARIO 30 — Unreachable objects in the heap dump (GC allocation lag)
 *
 * ─────────────────────────────────────────────────────────────────────────
 * KEY CONCEPT: two flavours of heap dump
 *
 *   jmap -dump:live,format=b,file=live.hprof   <pid>   ← full GC first → LIVE ONLY
 *   jmap -dump:format=b,file=all.hprof          <pid>   ← NO GC → ALL including GARBAGE
 *   jcmd <pid> GC.heap_dump -all=true all.hprof         ← same as jmap without live
 *
 * A "live" dump is what you almost always want for leak investigation.
 * An "all" dump exposes objects that are already unreachable (no path from
 * any GC root) but have not yet been collected because:
 *   (a) The GC cycle that would collect them hasn't run yet.
 *   (b) Concurrent GC (G1/ZGC) is partway through a marking cycle.
 *   (c) The objects are in the finalizer queue (scenario 10).
 *   (d) The young generation has not been evacuated since the objects died.
 *
 * ─────────────────────────────────────────────────────────────────────────
 * SETUP
 *
 * Run:   java -Xmx256m -Xms256m \
 *             -XX:+UseSerialGC \
 *             -XX:NewSize=160m -XX:MaxNewSize=160m \
 *             30_GcOverwhelmedGarbage.java
 *
 * With SerialGC and a fixed 160 MB young generation:
 *   - GC fires only when an allocation cannot fit in the young gen.
 *   - Between collections the young gen is full of a mix of live and dead objects.
 *   - We can reliably capture unreachable objects during this window.
 *
 * ─────────────────────────────────────────────────────────────────────────
 * WHAT TO DUMP
 *
 * After the program prints "DUMP WINDOW OPEN", run in a separate terminal:
 *
 *   PID=$(pgrep -f GcOverwhelmedGarbage)
 *
 *   # Dump A — all objects including unreachable:
 *   jmap -dump:format=b,file=heap_all.hprof   $PID
 *
 *   # Dump B — live objects only (full GC first):
 *   jmap -dump:live,format=b,file=heap_live.hprof  $PID
 *
 *   # Compare file sizes (heap_all should be noticeably larger):
 *   ls -lh heap_all.hprof heap_live.hprof
 *
 * ─────────────────────────────────────────────────────────────────────────
 * ANALYSIS
 *
 *   # Compare histograms between the two dumps:
 *   hprof-analyzer heap histogram heap_all.hprof  > all_hist.txt
 *   hprof-analyzer heap histogram heap_live.hprof > live_hist.txt
 *   diff all_hist.txt live_hist.txt
 *
 *   # Classes with MORE instances in heap_all than heap_live
 *   # → these are the unreachable objects still sitting in the heap
 *
 *   hprof-analyzer heap query heap_all.hprof \
 *       --oql "SELECT @displayName, COUNT(*) AS n \
 *               FROM INSTANCEOF java.lang.Object \
 *               GROUP BY @displayName ORDER BY n DESC LIMIT 20"
 *
 *   # Specifically look for GarbageRequest instances:
 *   hprof-analyzer heap query heap_all.hprof \
 *       --oql "SELECT COUNT(*) FROM _30_GcOverwhelmedGarbage\$GarbageRequest"
 *   # vs
 *   hprof-analyzer heap query heap_live.hprof \
 *       --oql "SELECT COUNT(*) FROM _30_GcOverwhelmedGarbage\$GarbageRequest"
 *
 *   # GarbageRequests present in heap_all but absent in heap_live = unreachable garbage
 *
 * ─────────────────────────────────────────────────────────────────────────
 * WHAT DOES hprof-analyzer DO WITH UNREACHABLE OBJECTS?
 *
 * Objects with no path from any GC root appear in:
 *   - The class HISTOGRAM (count + shallow size) — they look like any other object.
 *   - The DOMINATOR TREE: unreachable objects have no dominator (or are their own
 *     root); they typically do NOT appear in the leak suspects report.
 *   - OQL queries: SELECT ... FROM ClassName returns them.
 *
 * This means: if you run a leak analysis on heap_all.hprof, the GarbageRequests
 * will inflate the histogram but will NOT be flagged as leak suspects because
 * they have no GC root chain. This tests whether the analyzer correctly separates
 * "live but leaking" from "already dead but not yet collected."
 *
 * ─────────────────────────────────────────────────────────────────────────
 * ALTERNATIVE: Trigger the scenario automatically via OOM
 *
 * Run without the sleep to trigger OOM, where the dump is taken mid-cycle:
 *
 *   java -Xmx64m -XX:+UseSerialGC \
 *        -XX:+HeapDumpOnOutOfMemoryError -XX:HeapDumpPath=heap_oom.hprof \
 *        30_GcOverwhelmedGarbage.java oom
 *
 * The OOM dump is taken before the JVM retries GC, so it may include
 * objects that GC marked as unreachable but has not yet swept.
 *
 * ─────────────────────────────────────────────────────────────────────────
 * GC LOG (to observe collection cycles)
 *
 *   java ... -Xlog:gc*:gc.log 30_GcOverwhelmedGarbage.java
 *   # Watch minor GCs and when they fire vs the dump window
 */
import java.util.*;

public class _30_GcOverwhelmedGarbage {

    // ── Object that will become garbage ──────────────────────────────────
    static class GarbageRequest {
        final String requestId;
        final byte[] payload       = new byte[512];      // 512 B
        final Map<String, String> headers = new HashMap<>(4);
        final List<String> tags;

        GarbageRequest(int id) {
            this.requestId = "req-" + id;
            this.headers.put("Content-Type",  "application/json");
            this.headers.put("Authorization", "Bearer tok-" + (id % 100));
            this.headers.put("X-Request-Id",  requestId);
            this.tags = Arrays.asList("api-v2", "region-" + (id % 8));
        }
    }

    // ── Object that stays LIVE throughout ────────────────────────────────
    static class LiveSession {
        final int id;
        final byte[] data = new byte[2_048]; // 2 KB
        LiveSession(int id) { this.id = id; }
    }

    // Global live set — never collected
    static final List<LiveSession> LIVE_SESSIONS = new ArrayList<>();

    // ── Helpers ───────────────────────────────────────────────────────────

    /** Fill ~60 MB with permanently-live LiveSession objects. */
    static void populateLiveSessions(int count) {
        for (int i = 0; i < count; i++) {
            LIVE_SESSIONS.add(new LiveSession(i));
        }
        Runtime rt = Runtime.getRuntime();
        System.gc();
        System.out.printf("  Live sessions: %,d  heap used: %,d MB / %,d MB%n",
            LIVE_SESSIONS.size(),
            (rt.totalMemory() - rt.freeMemory()) >> 20,
            rt.maxMemory() >> 20);
    }

    /**
     * Create a wave of GarbageRequest objects.
     * The returned list is the ONLY reference. Caller drops it → all unreachable.
     */
    static List<GarbageRequest> createGarbageWave(int n) {
        List<GarbageRequest> wave = new ArrayList<>(n);
        for (int i = 0; i < n; i++) {
            wave.add(new GarbageRequest(i));
        }
        return wave;
    }

    /** Continuous allocation + discard to stress the GC (OOM variant). */
    static void stressAllocate() {
        //noinspection InfiniteLoopStatement
        while (true) {
            // Each iteration: ~600 B of garbage, never held
            GarbageRequest r = new GarbageRequest((int)(Math.random() * 1_000_000));
            // r is unreachable immediately — GC must collect faster than we produce
            if (r.requestId.isEmpty()) break; // prevent optimizer eliding r
        }
    }

    // ── Main ──────────────────────────────────────────────────────────────

    public static void main(String[] args) throws Exception {

        boolean oomMode = args.length > 0 && args[0].equals("oom");

        System.out.println("=== GC Overwhelmed Garbage Demo ===");
        System.out.println("Mode: " + (oomMode ? "OOM (automatic dump)" : "MANUAL DUMP WINDOW"));
        System.out.println();

        // ── Phase 1: establish a permanent live set ──────────────────────
        System.out.println("Phase 1: Building permanent live set (~30 000 sessions × 2 KB) …");
        populateLiveSessions(30_000); // ~60 MB live

        if (oomMode) {
            // ── OOM mode: stress until heap exhausted ───────────────────
            System.out.println("Phase 2 (OOM mode): stress-allocating until OutOfMemoryError …");
            System.out.println("  GC will retry and fail; OOM dump captured mid-cycle.");
            stressAllocate(); // never returns normally

        } else {
            // ── Normal mode: create unreachable wave, then hold for dump ─
            System.out.println("Phase 2: Creating garbage wave of 150 000 GarbageRequests …");
            List<GarbageRequest> wave = createGarbageWave(150_000);

            // Each GarbageRequest ≈ 512 B payload + ~200 B header map + ~100 B tags
            // ≈ ~800 B shallow, ~1.2 KB total per object → wave ≈ ~180 MB
            // With -Xmx256m this fills much of the heap.

            System.out.println("Phase 3: Dropping reference to wave → all 150 000 objects NOW UNREACHABLE.");
            //noinspection UnusedAssignment
            wave = null; // <── objects are unreachable from this point

            Runtime rt = Runtime.getRuntime();
            System.out.printf("  Heap used (before GC): %,d MB / %,d MB%n",
                (rt.totalMemory() - rt.freeMemory()) >> 20,
                rt.maxMemory() >> 20);
            System.out.println();

            System.out.println("╔═══════════════════════════════════════════════════════════╗");
            System.out.println("║         DUMP WINDOW OPEN — run in another terminal:        ║");
            System.out.println("╠═══════════════════════════════════════════════════════════╣");
            System.out.printf ("║  PID=$(pgrep -f GcOverwhelmedGarbage)%n");
            System.out.println("║                                                             ║");
            System.out.println("║  # All objects (incl. unreachable GarbageRequests):        ║");
            System.out.println("║  jmap -dump:format=b,file=heap_all.hprof  $PID             ║");
            System.out.println("║                                                             ║");
            System.out.println("║  # Live objects only (full GC first):                      ║");
            System.out.println("║  jmap -dump:live,format=b,file=heap_live.hprof $PID        ║");
            System.out.println("║                                                             ║");
            System.out.println("║  ls -lh heap_all.hprof heap_live.hprof                     ║");
            System.out.println("╚═══════════════════════════════════════════════════════════╝");
            System.out.println();
            System.out.println("Waiting 5 min for dump … (Ctrl-C to exit)");

            // Sleep for a long time to give you a chance to dump.
            // The GC may eventually collect the wave even without our help.
            // Take the dump as quickly as possible after the print above.
            Thread.sleep(300_000);

            // Force GC after the window to show what the heap looks like WITHOUT garbage
            System.out.println();
            System.out.println("Window closed. Running System.gc() to collect the garbage …");
            System.gc();
            Thread.sleep(2_000);
            System.out.printf("  Heap used (after GC):  %,d MB / %,d MB%n",
                (rt.totalMemory() - rt.freeMemory()) >> 20,
                rt.maxMemory() >> 20);
            System.out.println("Now take heap_after_gc.hprof and compare to heap_all.hprof.");
            System.out.println(LIVE_SESSIONS.size()); // prevent optimizer
        }
    }
}

/**
 * NORMAL APP 33 — Event stream pipeline with sliding-window aggregation
 *
 * A realistic stream-processing application with a sliding window, per-bucket
 * aggregates, and a rule engine. No leaks — the window is bounded.
 * Use it to explore what time-series event data looks like on the heap.
 *
 * Run:   java -Xmx256m 33_AppEventPipeline.java
 *
 * Capture (after "HEAP_DUMP_READY" is printed):
 *   jcmd $(pgrep -f AppEventPipeline) GC.heap_dump heap.hprof
 *
 * What's in the heap:
 *   50 000 Event objects in the sliding window   → id, type, source, ts, payload
 *   Per-minute bucket aggregates                 → TreeMap<Long, BucketStats>
 *   Per-type aggregates                          → HashMap<String, TypeStats>
 *   Per-source aggregates                        → HashMap<String, SourceStats>
 *   20 Alert rules (lambda predicates + metadata)
 *   Ring buffer of last 1 000 alerts triggered   → ArrayDeque<AlertRecord>
 *
 * Interesting analysis:
 *   hprof-analyzer heap histogram heap.hprof
 *   → Event[] / Deque internals, String objects for event types/sources
 *
 *   hprof-analyzer heap query heap.hprof \
 *       --oql "SELECT COUNT(*) FROM _33_AppEventPipeline\$Event"
 *
 *   hprof-analyzer heap report heap.hprof --section overview
 *   → Object graph centred on PIPELINE singleton
 *
 *   hprof-analyzer heap.hprof report.html --collections
 *   → TreeMap (bucket index) fill ratio, ArrayDeque (alerts ring buffer)
 */
import java.util.*;
import java.util.function.*;

public class _33_AppEventPipeline {

    // ── Event model ───────────────────────────────────────────────────────
    static final String[] TYPES   = {"click","view","purchase","error","login",
                                      "logout","search","add_cart","remove_cart","checkout"};
    static final String[] SOURCES = {"web","mobile-ios","mobile-android","api","partner",
                                      "batch","webhook","cron","admin","sdk"};

    static class Event {
        final long   id;
        final String type;
        final String source;
        final long   timestamp;     // epoch ms
        final byte[] payload;       // simulated event body

        Event(long id, Random rng) {
            this.id        = id;
            this.type      = TYPES[rng.nextInt(TYPES.length)];
            this.source    = SOURCES[rng.nextInt(SOURCES.length)];
            this.timestamp = System.currentTimeMillis() - rng.nextInt(3_600_000); // last hour
            this.payload   = new byte[64 + rng.nextInt(192)]; // 64–256 B
            rng.nextBytes(this.payload);
        }
    }

    // ── Aggregate buckets ─────────────────────────────────────────────────
    static class BucketStats {
        long count;
        long errorCount;
        long totalPayloadBytes;

        void record(Event e) {
            count++;
            totalPayloadBytes += e.payload.length;
            if ("error".equals(e.type)) errorCount++;
        }
    }

    static class TypeStats {
        final String type;
        long count;
        long lastSeen;

        TypeStats(String type) { this.type = type; }

        void record(Event e) { count++; lastSeen = e.timestamp; }
    }

    static class SourceStats {
        final String source;
        long count;
        long errorCount;
        long avgPayloadBytes;
        long sampleCount;

        SourceStats(String source) { this.source = source; }

        void record(Event e) {
            count++;
            if ("error".equals(e.type)) errorCount++;
            avgPayloadBytes = (avgPayloadBytes * sampleCount + e.payload.length) / (sampleCount + 1);
            sampleCount++;
        }
    }

    // ── Alert model ───────────────────────────────────────────────────────
    static class AlertRule {
        final String             name;
        final Predicate<Event>   condition;
        final String             message;

        AlertRule(String name, Predicate<Event> condition, String message) {
            this.name      = name;
            this.condition = condition;
            this.message   = message;
        }
    }

    static class AlertRecord {
        final String ruleName;
        final long   firedAt;
        final long   eventId;

        AlertRecord(AlertRule rule, Event e) {
            this.ruleName = rule.name;
            this.firedAt  = System.currentTimeMillis();
            this.eventId  = e.id;
        }
    }

    // ── Pipeline state ────────────────────────────────────────────────────
    static final int WINDOW_SIZE  = 50_000;
    static final int ALERT_BUFFER = 1_000;

    static final Deque<Event>                  WINDOW         = new ArrayDeque<>(WINDOW_SIZE + 1);
    static final TreeMap<Long, BucketStats>    MINUTE_BUCKETS = new TreeMap<>();
    static final Map<String, TypeStats>        TYPE_STATS     = new HashMap<>();
    static final Map<String, SourceStats>      SOURCE_STATS   = new HashMap<>();
    static final List<AlertRule>               RULES          = new ArrayList<>();
    static final Deque<AlertRecord>            ALERT_LOG      = new ArrayDeque<>();

    static void initRules() {
        RULES.add(new AlertRule("high-error-rate",
            e -> "error".equals(e.type),
            "Error event detected"));
        RULES.add(new AlertRule("checkout-from-mobile",
            e -> "checkout".equals(e.type) && e.source.startsWith("mobile"),
            "Mobile checkout"));
        RULES.add(new AlertRule("large-payload",
            e -> e.payload.length > 200,
            "Unusually large event payload"));
        RULES.add(new AlertRule("login-from-api",
            e -> "login".equals(e.type) && "api".equals(e.source),
            "API login — possible bot"));
        RULES.add(new AlertRule("purchase-success",
            e -> "purchase".equals(e.type),
            "Purchase completed"));
        // … 15 more rules (same pattern, different conditions)
        for (int i = 5; i < 20; i++) {
            final String src = SOURCES[i % SOURCES.length];
            final String typ = TYPES[i % TYPES.length];
            RULES.add(new AlertRule("rule-" + i,
                e -> e.source.equals(src) && e.type.equals(typ),
                "Pattern " + i + " matched"));
        }
    }

    static void ingest(Event e) {
        // Maintain bounded sliding window
        WINDOW.addLast(e);
        if (WINDOW.size() > WINDOW_SIZE) WINDOW.pollFirst();

        // Per-minute bucket (key = epoch-second / 60)
        long bucket = e.timestamp / 60_000;
        MINUTE_BUCKETS.computeIfAbsent(bucket, k -> new BucketStats()).record(e);

        // Per-type and per-source aggregates
        TYPE_STATS.computeIfAbsent(e.type, TypeStats::new).record(e);
        SOURCE_STATS.computeIfAbsent(e.source, SourceStats::new).record(e);

        // Evaluate alert rules
        for (AlertRule rule : RULES) {
            if (rule.condition.test(e)) {
                ALERT_LOG.addLast(new AlertRecord(rule, e));
                if (ALERT_LOG.size() > ALERT_BUFFER) ALERT_LOG.pollFirst();
            }
        }
    }

    public static void main(String[] args) throws Exception {
        System.out.println("=== Event Stream Pipeline ===");
        initRules();

        System.out.println("Ingesting " + WINDOW_SIZE + " events into sliding window …");
        Random rng = new Random(7);
        for (long i = 0; i < WINDOW_SIZE; i++) {
            ingest(new Event(i, rng));
        }

        Runtime rt = Runtime.getRuntime();
        System.gc(); Thread.sleep(200);
        System.out.printf("Ready | heap %,d MB / %,d MB%n",
            (rt.totalMemory() - rt.freeMemory()) >> 20,
            rt.maxMemory() >> 20);
        System.out.printf("  window:         %,6d events%n",  WINDOW.size());
        System.out.printf("  minute buckets: %,6d%n",         MINUTE_BUCKETS.size());
        System.out.printf("  type stats:     %,6d types%n",   TYPE_STATS.size());
        System.out.printf("  source stats:   %,6d sources%n", SOURCE_STATS.size());
        System.out.printf("  alert rules:    %,6d%n",         RULES.size());
        System.out.printf("  alerts fired:   %,6d%n",         ALERT_LOG.size());
        System.out.println();
        System.out.println("HEAP_DUMP_READY");
        System.out.println("  jcmd $(pgrep -f AppEventPipeline) GC.heap_dump heap.hprof");
        Thread.sleep(Long.MAX_VALUE);
    }
}

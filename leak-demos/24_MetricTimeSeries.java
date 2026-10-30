/**
 * SCENARIO 24 — Normal time-series metric store: many uniform small objects
 *
 * THIS IS NOT A BUG. The application correctly stores recent metric samples
 * in a rolling window per metric. The point is to show what a healthy
 * time-series in-memory store looks like in a heap dump, and to test how
 * hprof-analyzer handles thousands of uniform objects.
 *
 * Run:   java -Xmx128m 24_MetricTimeSeries.java
 *        (No OOM intended. Heap dump mid-run.)
 *
 * Capture:  jcmd $(pgrep -f MetricTimeSeries) GC.heap_dump heap.hprof
 *
 * What to find:
 *   - DataPoint[] arrays are the single largest class by total shallow size
 *   - All DataPoint instances have identical shallow size (double ts + double val = ~32 B)
 *   - MetricStore retains everything via its HashMap<String, CircularBuffer>
 *   - CircularBuffer[] contributes a lot of Object[] overhead
 *   - The histogram shows many instances, all small — "normal" for a metrics system
 *
 * hprof-analyzer:
 *   hprof-analyzer heap histogram heap.hprof
 *   → DataPoint as one of the top classes by count
 *
 *   hprof-analyzer heap query heap.hprof \
 *       --oql "SELECT COUNT(*) FROM _24_MetricTimeSeries\$DataPoint"
 *
 *   hprof-analyzer heap query heap.hprof \
 *       --oql "SELECT @displayName, COUNT(*) AS n FROM INSTANCEOF java.lang.Object
 *               GROUP BY @displayName ORDER BY n DESC LIMIT 10"
 *
 *   hprof-analyzer heap report heap.hprof --section overview
 *   → Compact class distribution; no single suspect dominates
 *
 *   # Check for over-allocation in CircularBuffers:
 *   hprof-analyzer heap.hprof report.html --collections
 */
import java.util.*;

public class _24_MetricTimeSeries {

    static final class DataPoint {
        final double timestamp; // epoch seconds
        final double value;
        DataPoint(double ts, double v) { this.timestamp = ts; this.value = v; }
    }

    // Fixed-size circular buffer of DataPoints
    static final class CircularBuffer {
        private final DataPoint[] data;
        private int head = 0;
        private int size = 0;

        CircularBuffer(int capacity) { this.data = new DataPoint[capacity]; }

        void add(double ts, double value) {
            data[head] = new DataPoint(ts, value);
            head = (head + 1) % data.length;
            if (size < data.length) size++;
        }

        int size() { return size; }
    }

    static final class MetricStore {
        // metricName → circular buffer of last N samples
        private final Map<String, CircularBuffer> store = new HashMap<>();
        private final int windowSize;

        MetricStore(int windowSize) { this.windowSize = windowSize; }

        void record(String metric, double timestamp, double value) {
            store.computeIfAbsent(metric, k -> new CircularBuffer(windowSize))
                 .add(timestamp, value);
        }

        int metricCount() { return store.size(); }
        long totalSamples() {
            return store.values().stream().mapToLong(CircularBuffer::size).sum();
        }
    }

    public static void main(String[] args) throws Exception {
        // Simulate a monitoring agent: 2 000 metric names, 1 000 samples each
        int METRIC_COUNT  = 2_000;
        int SAMPLES_EACH  = 1_000;
        int WINDOW        = 1_000; // keep last 1000 samples

        MetricStore store = new MetricStore(WINDOW);
        Random rng = new Random(42);

        System.out.printf("Recording %,d metrics × %,d samples …%n", METRIC_COUNT, SAMPLES_EACH);

        double baseTs = 1_700_000_000.0;
        for (int s = 0; s < SAMPLES_EACH; s++) {
            for (int m = 0; m < METRIC_COUNT; m++) {
                String metric = "host" + (m / 100) + ".cpu" + (m % 100);
                store.record(metric, baseTs + s, 10.0 + rng.nextDouble() * 80.0);
            }
            if (s % 200 == 0) {
                Runtime rt = Runtime.getRuntime();
                System.out.printf("  sample=%,d  metrics=%,d  totalPoints=%,d  heap=%,d MB%n",
                    s, store.metricCount(), store.totalSamples(),
                    (rt.totalMemory() - rt.freeMemory()) >> 20);
            }
        }

        Runtime rt = Runtime.getRuntime();
        System.gc(); Thread.sleep(300);
        System.out.printf("%nStore: %,d metrics, %,d total samples, heap=%,d MB / %,d MB%n",
            store.metricCount(), store.totalSamples(),
            (rt.totalMemory() - rt.freeMemory()) >> 20,
            rt.maxMemory() >> 20);
        System.out.printf("Expected DataPoint count: %,d%n",
            (long) METRIC_COUNT * SAMPLES_EACH);
        System.out.println();
        System.out.println("Take a heap dump now:");
        System.out.println("  jcmd $(pgrep -f MetricTimeSeries) GC.heap_dump heap.hprof");
        System.out.println("Then: hprof-analyzer heap histogram heap.hprof");

        Thread.sleep(Long.MAX_VALUE);
    }
}

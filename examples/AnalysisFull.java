import hprof.*;

import java.util.*;
import java.util.stream.*;

/**
 * Complete heap dump investigation using the Java scripting API.
 *
 * Usage:
 *   hprof-analyzer heap script java dump.hprof --file AnalysisFull.java
 *   hprof-analyzer heap script java dump.hprof --file AnalysisFull.java --with-graph
 *
 * This script demonstrates analysis that OQL fundamentally cannot do:
 * multi-step traversals, cross-class correlation, statistical outlier
 * detection, package-level aggregation, graph neighbourhood analysis,
 * and computed metrics (percentages, ratios, distributions).
 */
public class AnalysisFull implements HprofScript {

    @Override
    public void run(HprofSession s) throws Exception {

        // ═══════════════════════════════════════════════════════════════
        // 1. TRIAGE
        // ═══════════════════════════════════════════════════════════════
        section(1, "TRIAGE");
        var info = s.dumpInfo();
        System.out.println(info);
        long heapBytes = info.heapBytes();
        System.out.println();

        // ═══════════════════════════════════════════════════════════════
        // 2. CLASS HISTOGRAM with heap-% (OQL cannot compute percentages)
        // ═══════════════════════════════════════════════════════════════
        section(2, "CLASS HISTOGRAM (top 15 by retained, with heap%)");
        var histogram = s.classHistogram(15);
        for (int i = 0; i < histogram.length; i++) {
            var h = histogram[i];
            System.out.printf("  %2d. %-50s %8s instances  %10s retained  (%s)%n",
                i + 1, h.className(), Fmt.integer(h.instances()),
                Fmt.bytes(h.retainedBytes()), Fmt.pct(h.retainedBytes(), heapBytes));
        }
        System.out.println();

        // ═══════════════════════════════════════════════════════════════
        // 3. OUTLIER DETECTION (OQL cannot compare across instances)
        // ═══════════════════════════════════════════════════════════════
        section(3, "OUTLIER DETECTION (classes where max > 5x average)");
        var stats = s.classStats(100);
        var outliers = Arrays.stream(stats)
            .filter(cs -> cs.isOutlier(5.0))
            .sorted(Comparator.comparingDouble(ClassStats::outlierRatio).reversed())
            .toList();

        if (outliers.isEmpty()) {
            System.out.println("  (no outliers found)");
        } else {
            for (var o : outliers) {
                System.out.printf("  %-45s %6s instances  avg=%10s  max=%10s  (%.0fx, idx=%d)%n",
                    o.className(), Fmt.integer(o.instances()),
                    Fmt.bytes(o.avgRetained()), Fmt.bytes(o.maxRetained()),
                    o.outlierRatio(), o.maxRetainedIdx());
            }
        }
        System.out.println();

        // ═══════════════════════════════════════════════════════════════
        // 4. TOP OBJECTS with deep inspection
        // ═══════════════════════════════════════════════════════════════
        section(4, "TOP 10 OBJECTS (with dominator depth + retained%)");
        var tops = s.topObjects(10);
        for (int i = 0; i < tops.length; i++) {
            var obj = tops[i];
            long depth = s.dominatorDepth(obj.idx());
            System.out.printf("  %2d. [%7d] %-42s %10s (%s of heap, depth %d)%n",
                i + 1, obj.idx(), obj.className(),
                Fmt.bytes(obj.retainedBytes()),
                Fmt.pct(obj.retainedBytes(), heapBytes), depth);
        }
        System.out.println();

        // ═══════════════════════════════════════════════════════════════
        // 5. ACCUMULATION POINT CASCADE
        //    OQL cannot: walk dominator trees, compute ratio, recurse
        // ═══════════════════════════════════════════════════════════════
        section(5, "ACCUMULATION POINTS (retained/shallow >= 10x, >= 1% of heap)");
        long threshold = heapBytes / 100;
        var suspects = s.accumulationPoints(threshold, 10.0, 10);
        for (int i = 0; i < suspects.length; i++) {
            var ap = suspects[i];
            System.out.printf("%n  %d. %s%n", i + 1, ap);

            // Dominator path
            var path = s.dominatorPath(ap.idx());
            if (path.length > 1) {
                System.out.println("     Dominator chain:");
                for (var step : path) {
                    System.out.printf("       -> %s%n", step);
                }
            }

            // Big drops (what this object holds)
            var drops = s.bigDrops(ap.idx(), 5);
            if (drops.length > 0) {
                System.out.println("     Biggest children:");
                for (var d : drops) {
                    System.out.printf("       +- %s%n", d);
                }
            }

            // Retained set class breakdown
            var rs = s.retainedSetSummary(ap.idx());
            if (rs.length > 0) {
                System.out.println("     Retained set (top 5 classes):");
                for (int j = 0; j < Math.min(5, rs.length); j++) {
                    System.out.printf("       %s%n", rs[j]);
                }
            }
        }
        System.out.println();

        // ═══════════════════════════════════════════════════════════════
        // 6. CHAIN DETECTION (OQL cannot detect linked-list patterns)
        // ═══════════════════════════════════════════════════════════════
        section(6, "SAME-CLASS CHAINS (linked-list leak patterns)");
        var chains = s.detectChains(3, 15);
        if (chains.length == 0) {
            System.out.println("  (no chains of length >= 3)");
        } else {
            for (var c : chains) {
                System.out.printf("  %s%n", c);
            }
        }
        System.out.println();

        // ═══════════════════════════════════════════════════════════════
        // 7. CROSS-CLASS RETENTION COMPARISON
        //    OQL cannot: compare who_retains across multiple classes
        // ═══════════════════════════════════════════════════════════════
        section(7, "RETENTION COMPARISON (who holds byte[], String, HashMap)");
        for (String cls : List.of("byte[]", "java.lang.String", "HashMap")) {
            var total = s.classTotal(cls);
            System.out.printf("  %s: %s instances, %s retained (%s of heap)%n",
                cls, Fmt.integer(total.instances()),
                Fmt.bytes(total.retainedBytes()), Fmt.pct(total.retainedBytes(), heapBytes));

            var retainers = s.whoRetains(cls, 5);
            for (var r : retainers) {
                System.out.printf("    held by %-45s  %5s instances%n", r.className(), Fmt.integer(r.instanceCount()));
            }
            System.out.println();
        }

        // ═══════════════════════════════════════════════════════════════
        // 8. PACKAGE-LEVEL AGGREGATION
        //    OQL cannot: group by computed prefix / aggregate across classes
        // ═══════════════════════════════════════════════════════════════
        section(8, "PACKAGE-LEVEL MEMORY ROLLUP");
        var pkgMap = new LinkedHashMap<String, long[]>(); // [instances, retained]
        for (var h : s.classHistogram(200)) {
            String pkg = packageOf(h.className());
            pkgMap.computeIfAbsent(pkg, k -> new long[2]);
            pkgMap.get(pkg)[0] += h.instances();
            pkgMap.get(pkg)[1] += h.retainedBytes();
        }
        pkgMap.entrySet().stream()
            .sorted(Comparator.comparingLong((Map.Entry<String, long[]> e) -> e.getValue()[1]).reversed())
            .limit(15)
            .forEach(e -> System.out.printf("  %-40s %8s instances  %10s retained  (%s)%n",
                e.getKey(), Fmt.integer(e.getValue()[0]),
                Fmt.bytes(e.getValue()[1]), Fmt.pct(e.getValue()[1], heapBytes)));
        System.out.println();

        // ═══════════════════════════════════════════════════════════════
        // 9. COMMON DOMINATOR CLUSTERING
        //    OQL cannot: compute LCA across multiple objects
        // ═══════════════════════════════════════════════════════════════
        section(9, "COMMON DOMINATOR (top 3 objects)");
        if (tops.length >= 3) {
            long lca01 = s.commonDominator(tops[0].idx(), tops[1].idx());
            long lca02 = s.commonDominator(tops[0].idx(), tops[2].idx());
            long lca12 = s.commonDominator(tops[1].idx(), tops[2].idx());
            System.out.printf("  [%d] & [%d] -> LCA [%d] %s (%s)%n",
                tops[0].idx(), tops[1].idx(), lca01, s.className(lca01), Fmt.bytes(s.retained(lca01)));
            System.out.printf("  [%d] & [%d] -> LCA [%d] %s (%s)%n",
                tops[0].idx(), tops[2].idx(), lca02, s.className(lca02), Fmt.bytes(s.retained(lca02)));
            System.out.printf("  [%d] & [%d] -> LCA [%d] %s (%s)%n",
                tops[1].idx(), tops[2].idx(), lca12, s.className(lca12), Fmt.bytes(s.retained(lca12)));
        }
        System.out.println();

        // ═══════════════════════════════════════════════════════════════
        // 10. REFERENCE GRAPH (if loaded)
        // ═══════════════════════════════════════════════════════════════
        if (s.isGraphLoaded() && tops.length >= 2) {
            section(10, "REFERENCE GRAPH ANALYSIS");
            long a = tops[0].idx(), b = tops[1].idx();
            var path = s.referencePath(a, b, 20);
            if (path.length > 0) {
                System.out.printf("  Path [%d] -> [%d] (%d hops):%n", a, b, path.length - 1);
                for (int i = 0; i < path.length; i++) {
                    long idx = path[i];
                    System.out.printf("    %d. [%d] %s (%s)%n",
                        i + 1, idx, s.className(idx), Fmt.bytes(s.retained(idx)));
                }
            } else {
                System.out.printf("  No path from [%d] to [%d] within 20 hops%n", a, b);
            }

            // Incoming refs for byte[]
            System.out.println();
            System.out.println("  Incoming reference sources for byte[]:");
            var incoming = s.incomingByClass("byte[]", 10);
            for (var e : incoming) {
                System.out.printf("    %s%n", e);
            }
            System.out.println();
        }

        // ═══════════════════════════════════════════════════════════════
        // 11. SUBTREE WALK
        //     OQL cannot: BFS traverse the dominator tree
        // ═══════════════════════════════════════════════════════════════
        section(11, "DOMINATOR SUBTREE of top object (depth 3)");
        if (tops.length > 0) {
            var nodes = s.walkSubtree(tops[0].idx(), 3, 30);
            for (var n : nodes) {
                System.out.println("  " + n);
            }
        }
        System.out.println();

        // ═══════════════════════════════════════════════════════════════
        // 12. OQL + ENRICHMENT (hybrid scripting)
        //     Run OQL, then enrich each row with data OQL cannot produce
        // ═══════════════════════════════════════════════════════════════
        section(12, "OQL + ENRICHMENT (HashMap instances + dominator depth + retained%)");
        String oqlJson = s.oqlJson(
            "SELECT @objectId AS idx, @retainedHeapSize AS ret " +
            "FROM java.util.HashMap ORDER BY ret DESC LIMIT 10");
        System.out.println("  OQL returned JSON (first 200 chars):");
        System.out.println("  " + oqlJson.substring(0, Math.min(200, oqlJson.length())));
        System.out.println();

        // ═══════════════════════════════════════════════════════════════
        // 13. REPORT SECTIONS
        // ═══════════════════════════════════════════════════════════════
        section(13, "REPORT SECTIONS");
        var sections = s.reportSectionNames();
        System.out.println("  Available: " + String.join(", ", sections));
        System.out.println();
        System.out.println("  Triage (first 300 chars):");
        String triage = s.reportSection("triage");
        System.out.println("  " + triage.substring(0, Math.min(300, triage.length())));
        System.out.println();

        // ═══════════════════════════════════════════════════════════════
        // 14. DOMINATOR DEPTH DISTRIBUTION
        //     OQL cannot: compute distributions over graph properties
        // ═══════════════════════════════════════════════════════════════
        section(14, "DOMINATOR DEPTH DISTRIBUTION (sampled from top 200 objects)");
        var depthHist = new TreeMap<Long, Integer>();
        for (var obj : s.topObjects(200)) {
            long depth = s.dominatorDepth(obj.idx());
            depthHist.merge(depth, 1, Integer::sum);
        }
        for (var entry : depthHist.entrySet()) {
            String bar = "#".repeat(Math.min(entry.getValue(), 60));
            System.out.printf("  depth %2d: %3d objects  %s%n",
                entry.getKey(), entry.getValue(), bar);
        }
        System.out.println();

        // ═══════════════════════════════════════════════════════════════
        // 15. SUMMARY
        // ═══════════════════════════════════════════════════════════════
        section(15, "SUMMARY");
        System.out.printf("  Heap          : %s across %s objects%n",
            Fmt.bytes(heapBytes), Fmt.integer(info.objectCount()));
        System.out.printf("  Outliers      : %d classes with max > 5x avg%n", outliers.size());
        System.out.printf("  Accum. points : %d (retained/shallow >= 10x, >= 1%% heap)%n", suspects.length);
        System.out.printf("  Chains        : %d (length >= 3)%n", chains.length);
        System.out.printf("  Graph         : %s%n", s.isGraphLoaded() ? "loaded" : "not loaded");
        System.out.println("\n  ANALYSIS COMPLETE");
    }

    // ── Helpers ──────────────────────────────────────────────────────────

    static void section(int n, String title) {
        String sep = "═".repeat(64);
        System.out.printf("%n%s%n  %d. %s%n%s%n", sep, n, title, sep);
    }

    static String packageOf(String className) {
        // "java.util.HashMap" -> "java.util"
        // "byte[]" -> "(primitives)"
        if (className.contains("[]")) return "(arrays)";
        int dot = className.lastIndexOf('.');
        return dot < 0 ? "(default)" : className.substring(0, dot);
    }
}

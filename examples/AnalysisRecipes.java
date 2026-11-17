import hprof.*;

import java.util.*;
import java.util.stream.*;

/**
 * 15 cookbook recipes showing what Java scripting can do that OQL cannot.
 *
 * Usage:
 *   hprof-analyzer heap script java dump.hprof --file AnalysisRecipes.java
 *
 * Each recipe is a static method — copy the one you need into your own script.
 */
public class AnalysisRecipes implements HprofScript {

    @Override
    public void run(HprofSession s) {
        recipe1_triage(s);
        recipe2_outlierHunting(s);
        recipe3_cascadeDig(s);
        recipe4_crossClassRetention(s);
        recipe5_packageRollup(s);
        recipe6_chainCorrelation(s);
        recipe7_depthDistribution(s);
        recipe8_retainedSetDiff(s);
        recipe9_collectionWaste(s);
        recipe10_oqlEnrichment(s);
        recipe11_sharedRetention(s);
        recipe12_graphNeighbourhood(s);
        recipe13_reportMining(s);
        recipe14_bigArrayAnalysis(s);
        recipe15_fullTriagePipeline(s);
    }

    // ── RECIPE 1: Quick triage with stream-computed percentages ──────────
    static void recipe1_triage(HprofSession s) {
        System.out.println("── RECIPE 1: QUICK TRIAGE ──");
        var info = s.dumpInfo();
        System.out.println(info);
        // Top 5 classes as percentage of heap — OQL can't compute %
        long heapBytes = info.heapBytes();
        Arrays.stream(s.classHistogram(5))
            .forEach(h -> System.out.printf("  %-45s %10s (%s)%n",
                h.className(), Fmt.bytes(h.retainedBytes()),
                Fmt.pct(h.retainedBytes(), heapBytes)));
        System.out.println();
    }

    // ── RECIPE 2: Statistical outlier hunting ───────────────────────────
    static void recipe2_outlierHunting(HprofSession s) {
        System.out.println("── RECIPE 2: OUTLIER HUNTING ──");
        // Find instances that are 10x larger than their class average
        // OQL cannot compare an instance to the class-level statistics
        var outliers = Arrays.stream(s.classStats(200))
            .filter(cs -> cs.isOutlier(10.0))
            .sorted(Comparator.comparingDouble(ClassStats::outlierRatio).reversed())
            .limit(10)
            .toList();
        for (var o : outliers) {
            System.out.printf("  %-40s max=%10s vs avg=%10s (%.0fx)%n",
                o.className(), Fmt.bytes(o.maxRetained()),
                Fmt.bytes(o.avgRetained()), o.outlierRatio());
            // Inspect the actual outlier object
            long depth = s.dominatorDepth(o.maxRetainedIdx());
            System.out.printf("    -> [%d] depth=%d, retained=%s%n",
                o.maxRetainedIdx(), depth, Fmt.bytes(s.retained(o.maxRetainedIdx())));
        }
        System.out.println();
    }

    // ── RECIPE 3: Multi-level dominator cascade ─────────────────────────
    static void recipe3_cascadeDig(HprofSession s) {
        System.out.println("── RECIPE 3: ACCUMULATION POINT CASCADE ──");
        // For each suspect: dig into children, then their children
        // OQL can only query one level at a time; this does recursive descent
        var suspects = s.accumulationPoints(s.heapTotalBytes() / 100, 10.0, 3);
        for (var ap : suspects) {
            System.out.printf("  %s%n", ap);
            var drops1 = s.bigDrops(ap.idx(), 3);
            for (var d1 : drops1) {
                System.out.printf("    +- %s%n", d1);
                // Recurse: second level
                var drops2 = s.bigDrops(d1.idx(), 2);
                for (var d2 : drops2) {
                    System.out.printf("      +- %s%n", d2);
                }
            }
            System.out.println();
        }
    }

    // ── RECIPE 4: Cross-class retention comparison ──────────────────────
    static void recipe4_crossClassRetention(HprofSession s) {
        System.out.println("── RECIPE 4: CROSS-CLASS RETENTION COMPARISON ──");
        // Compare who holds byte[] vs String vs HashMap — OQL can't correlate
        for (String cls : List.of("byte[]", "java.lang.String", "HashMap", "ArrayList")) {
            var total = s.classTotal(cls);
            if (total.instances() == 0) continue;
            System.out.printf("  %s: %s instances, %s retained%n",
                cls, Fmt.integer(total.instances()), Fmt.bytes(total.retainedBytes()));
            var retainers = s.whoRetains(cls, 3);
            for (var r : retainers) {
                System.out.printf("    <- %s (%s instances)%n", r.className(), Fmt.integer(r.instanceCount()));
            }
        }
        System.out.println();
    }

    // ── RECIPE 5: Package-level memory rollup ───────────────────────────
    static void recipe5_packageRollup(HprofSession s) {
        System.out.println("── RECIPE 5: PACKAGE-LEVEL MEMORY ROLLUP ──");
        // OQL can't group by computed prefix
        long heapBytes = s.heapTotalBytes();
        var byPkg = Arrays.stream(s.classHistogram(300))
            .collect(Collectors.groupingBy(
                h -> packageOf(h.className()),
                Collectors.summingLong(ClassHistogramEntry::retainedBytes)));
        byPkg.entrySet().stream()
            .sorted(Map.Entry.<String, Long>comparingByValue().reversed())
            .limit(10)
            .forEach(e -> System.out.printf("  %-35s %10s (%s)%n",
                e.getKey(), Fmt.bytes(e.getValue()), Fmt.pct(e.getValue(), heapBytes)));
        System.out.println();
    }

    // ── RECIPE 6: Chain + accumulation point correlation ────────────────
    static void recipe6_chainCorrelation(HprofSession s) {
        System.out.println("── RECIPE 6: CHAIN + ACCUMULATION CORRELATION ──");
        // Cross-reference chains with accumulation points
        var chains = s.detectChains(3, 20);
        var suspects = s.accumulationPoints(100000, 5.0, 50);
        var suspectClasses = Arrays.stream(suspects)
            .map(AccumulationPoint::className)
            .collect(Collectors.toSet());
        // Chains whose class also appears as an accumulation point = high-confidence leak
        var correlatedChains = Arrays.stream(chains)
            .filter(c -> suspectClasses.contains(c.className()))
            .toList();
        if (correlatedChains.isEmpty()) {
            System.out.println("  (no chains correlate with accumulation points)");
        } else {
            System.out.println("  HIGH-CONFIDENCE LEAKS (chain + accumulation point):");
            for (var c : correlatedChains) {
                System.out.printf("    %s%n", c);
            }
        }
        System.out.println();
    }

    // ── RECIPE 7: Dominator depth distribution ──────────────────────────
    static void recipe7_depthDistribution(HprofSession s) {
        System.out.println("── RECIPE 7: DOMINATOR DEPTH DISTRIBUTION ──");
        // OQL can't compute graph structural properties
        var depthHist = new TreeMap<Long, Integer>();
        for (var obj : s.topObjects(200)) {
            depthHist.merge(s.dominatorDepth(obj.idx()), 1, Integer::sum);
        }
        depthHist.forEach((depth, count) ->
            System.out.printf("  depth %2d: %3d  %s%n",
                depth, count, "#".repeat(Math.min(count, 50))));
        System.out.println();
    }

    // ── RECIPE 8: Retained set comparison ───────────────────────────────
    static void recipe8_retainedSetDiff(HprofSession s) {
        System.out.println("── RECIPE 8: RETAINED SET COMPARISON ──");
        // Compare what two suspects hold — OQL can't diff retained sets
        var suspects = s.accumulationPoints(s.heapTotalBytes() / 50, 10.0, 5);
        if (suspects.length >= 2) {
            var rs0 = Arrays.asList(s.retainedSetSummary(suspects[0].idx()));
            var rs1 = Arrays.asList(s.retainedSetSummary(suspects[1].idx()));
            var classes0 = rs0.stream().map(RetainedSetSummaryEntry::className).collect(Collectors.toSet());
            var classes1 = rs1.stream().map(RetainedSetSummaryEntry::className).collect(Collectors.toSet());
            var onlyIn0 = rs0.stream().filter(e -> !classes1.contains(e.className())).limit(5).toList();
            var onlyIn1 = rs1.stream().filter(e -> !classes0.contains(e.className())).limit(5).toList();
            System.out.printf("  Suspect A: %s%n", suspects[0]);
            System.out.printf("  Suspect B: %s%n", suspects[1]);
            System.out.printf("  Only in A (%d classes): %s%n", onlyIn0.size(),
                onlyIn0.stream().map(RetainedSetSummaryEntry::className).collect(Collectors.joining(", ")));
            System.out.printf("  Only in B (%d classes): %s%n", onlyIn1.size(),
                onlyIn1.stream().map(RetainedSetSummaryEntry::className).collect(Collectors.joining(", ")));
        } else {
            System.out.println("  (need >= 2 suspects for comparison)");
        }
        System.out.println();
    }

    // ── RECIPE 9: Collection waste detection ────────────────────────────
    static void recipe9_collectionWaste(HprofSession s) {
        System.out.println("── RECIPE 9: COLLECTION WASTE (big collections with small content) ──");
        // Use OQL to find large HashMaps, then compute shallow/retained ratio via scripting
        var maps = s.instancesOf("HashMap", 100);
        long overAllocated = Arrays.stream(maps)
            .filter(m -> m.ratio() > 50.0)  // retained 50x larger than shallow = holding a lot
            .count();
        System.out.printf("  %d of %d HashMaps have retained > 50x shallow (potential over-allocation)%n",
            overAllocated, maps.length);
        // Show top 5
        Arrays.stream(maps)
            .sorted(Comparator.comparingDouble(HeapObject::ratio).reversed())
            .limit(5)
            .forEach(m -> System.out.printf("    [%d] %s  ratio=%.0fx%n",
                m.idx(), Fmt.bytes(m.retainedBytes()), m.ratio()));
        System.out.println();
    }

    // ── RECIPE 10: OQL + script enrichment ──────────────────────────────
    static void recipe10_oqlEnrichment(HprofSession s) {
        System.out.println("── RECIPE 10: OQL + ENRICHMENT ──");
        // Run OQL, then enrich each result with dominator info OQL can't produce
        var objs = s.instancesOf("Thread", 50);
        System.out.printf("  %d Thread objects found%n", objs.length);
        for (int i = 0; i < Math.min(10, objs.length); i++) {
            var obj = objs[i];
            long depth = s.dominatorDepth(obj.idx());
            long parentIdx = s.idom(obj.idx());
            String parent = parentIdx >= 0 ? s.className(parentIdx) : "<root>";
            System.out.printf("    [%d] %s: %s retained, depth=%d, parent=%s%n",
                obj.idx(), obj.className(), Fmt.bytes(obj.retainedBytes()),
                depth, parent);
        }
        System.out.println();
    }

    // ── RECIPE 11: Shared retention clustering ──────────────────────────
    static void recipe11_sharedRetention(HprofSession s) {
        System.out.println("── RECIPE 11: SHARED RETENTION (common dominators) ──");
        // For top 5 objects, find pairwise common dominators
        // OQL cannot compute LCA
        var tops = s.topObjects(5);
        for (int i = 0; i < tops.length; i++) {
            for (int j = i + 1; j < tops.length; j++) {
                long lca = s.commonDominator(tops[i].idx(), tops[j].idx());
                if (lca >= 0) {
                    System.out.printf("  [%d] & [%d] -> LCA [%d] %s (%s)%n",
                        tops[i].idx(), tops[j].idx(), lca,
                        s.className(lca), Fmt.bytes(s.retained(lca)));
                }
            }
        }
        System.out.println();
    }

    // ── RECIPE 12: Object graph neighbourhood ───────────────────────────
    static void recipe12_graphNeighbourhood(HprofSession s) {
        System.out.println("── RECIPE 12: GRAPH NEIGHBOURHOOD ──");
        if (!s.isGraphLoaded()) {
            System.out.println("  (graph not loaded — use --with-graph)");
            System.out.println();
            return;
        }
        // Walk outbound + inbound refs of the biggest object — OQL can't traverse graphs
        var top = s.topObjects(1);
        if (top.length > 0) {
            long idx = top[0].idx();
            var outbound = s.outboundRefs(idx);
            var inbound = s.inboundRefs(idx);
            System.out.printf("  Object [%d] %s: %d outbound refs, %d inbound refs%n",
                idx, top[0].className(), outbound.length, inbound.length);
            // Summarise outbound by class
            var outClasses = new LinkedHashMap<String, Integer>();
            for (long ref : outbound) {
                outClasses.merge(s.className(ref), 1, Integer::sum);
            }
            System.out.println("  Outbound by class:");
            outClasses.entrySet().stream()
                .sorted(Map.Entry.<String, Integer>comparingByValue().reversed())
                .limit(5)
                .forEach(e -> System.out.printf("    %4d x %s%n", e.getValue(), e.getKey()));
        }
        System.out.println();
    }

    // ── RECIPE 13: Report section mining ────────────────────────────────
    static void recipe13_reportMining(HprofSession s) {
        System.out.println("── RECIPE 13: REPORT MINING ──");
        // Pull structured report data and cross-reference
        var sections = s.reportSectionNames();
        System.out.printf("  %d report sections available%n", sections.length);
        // Pull waste and leaks
        String waste = s.reportSection("waste");
        String leaks = s.reportSection("leaks");
        System.out.printf("  Waste section: %d chars%n", waste.length());
        System.out.printf("  Leaks section: %d chars%n", leaks.length());
        System.out.println();
    }

    // ── RECIPE 14: Big array analysis ───────────────────────────────────
    static void recipe14_bigArrayAnalysis(HprofSession s) {
        System.out.println("── RECIPE 14: BIG ARRAY ANALYSIS ──");
        // Find largest arrays and compute their retention ratio
        // OQL can find arrays but can't compute retention ratio
        for (String arrayType : List.of("byte[]", "char[]", "Object[]", "int[]")) {
            var instances = s.instancesOf(arrayType, 5);
            if (instances.length == 0) continue;
            System.out.printf("  %s (top 5):%n", arrayType);
            for (var obj : instances) {
                System.out.printf("    [%d] shallow=%s retained=%s ratio=%.1fx%n",
                    obj.idx(), Fmt.bytes(obj.shallowBytes()),
                    Fmt.bytes(obj.retainedBytes()), obj.ratio());
            }
        }
        System.out.println();
    }

    // ── RECIPE 15: Full automated triage pipeline ───────────────────────
    static void recipe15_fullTriagePipeline(HprofSession s) {
        System.out.println("── RECIPE 15: AUTOMATED TRIAGE PIPELINE ──");
        // Score the heap: combine multiple signals into a verdict
        long heapBytes = s.heapTotalBytes();
        int suspectCount = s.accumulationPoints(heapBytes / 100, 10.0, 100).length;
        int chainCount = s.detectChains(3, 100).length;
        int outlierCount = (int) Arrays.stream(s.classStats(100)).filter(cs -> cs.isOutlier(10.0)).count();

        // Simple scoring heuristic
        int score = suspectCount * 3 + chainCount * 5 + outlierCount * 2;
        String severity = score > 50 ? "CRITICAL" : score > 20 ? "WARNING" : score > 5 ? "MODERATE" : "HEALTHY";

        System.out.printf("  Heap: %s (%s objects)%n", Fmt.bytes(heapBytes), Fmt.integer(s.objectCount()));
        System.out.printf("  Accumulation points : %d (>= 1%% of heap)%n", suspectCount);
        System.out.printf("  Same-class chains   : %d (length >= 3)%n", chainCount);
        System.out.printf("  Statistical outliers: %d (max > 10x avg)%n", outlierCount);
        System.out.printf("  Score: %d -> %s%n", score, severity);
        System.out.println();
        System.out.println("── ALL 15 RECIPES COMPLETE ──");
    }

    // ── Helpers ──────────────────────────────────────────────────────────

    static String packageOf(String className) {
        if (className.contains("[]")) return "(arrays)";
        int dot = className.lastIndexOf('.');
        return dot < 0 ? "(default)" : className.substring(0, dot);
    }
}

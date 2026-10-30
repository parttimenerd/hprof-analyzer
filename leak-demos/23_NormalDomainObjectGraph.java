/**
 * SCENARIO 23 — Normal domain object graph: large in-memory product catalog
 *
 * THIS IS NOT A BUG. The application is doing exactly what it's supposed to do:
 * caching a product catalog in memory for fast lookup. The goal is to analyze
 * WHERE the memory goes, which classes dominate, and what hprof-analyzer reports
 * for a well-behaved (but large) object graph.
 *
 * Run:   java -Xmx256m 23_NormalDomainObjectGraph.java
 *        (No OOM intended. Heap dump mid-run.)
 *
 * Capture:  jcmd $(pgrep -f NormalDomainObjectGraph) GC.heap_dump heap.hprof
 *
 * What to find:
 *   - CATALOG (the root) retains ~100% of the heap via Product[] → Product instances
 *   - Product instances are the biggest class by total size in the histogram
 *   - Each Product retains its description byte[] (largest field per-object)
 *   - Review objects dominate the heap after the products themselves
 *   - Tag strings appear many times with potential duplicates
 *
 * hprof-analyzer:
 *   hprof-analyzer heap summary heap.hprof
 *   → Top classes by retained size; CATALOG at the very top
 *
 *   hprof-analyzer heap histogram heap.hprof
 *   → Product count, Review count, String count
 *
 *   hprof-analyzer heap report heap.hprof --section overview
 *   → Class breakdown by retained size
 *
 *   hprof-analyzer heap report heap.hprof --section leaks
 *   → CATALOG as the single large retainer (not a bug, just shows what's there)
 *
 *   hprof-analyzer heap query heap.hprof \
 *       --oql "SELECT COUNT(*) FROM _23_NormalDomainObjectGraph\$Product"
 *
 *   hprof-analyzer heap.hprof report.html --find-duplicates
 *   → Duplicate tag strings (many products share the same tag values)
 */
import java.util.*;
import java.nio.charset.StandardCharsets;

public class _23_NormalDomainObjectGraph {

    static class Review {
        final int     rating;    // 1-5
        final String  author;
        final String  text;
        Review(int rating, String author, String text) {
            this.rating = rating; this.author = author; this.text = text;
        }
    }

    static class Product {
        final int      id;
        final String   name;
        final String   category;
        final double   price;
        final byte[]   description;    // simulated rich-text blob, ~500 bytes each
        final String[] tags;           // 3-6 per product, repeated values across products
        final List<Review> reviews;

        Product(int id, Random rng) {
            this.id          = id;
            this.name        = "Product-" + id;
            this.category    = CATEGORIES[id % CATEGORIES.length];
            this.price       = 9.99 + (id % 1000);
            this.description = new byte[400 + rng.nextInt(200)]; // ~500 bytes
            rng.nextBytes(this.description);

            int tagCount = 3 + rng.nextInt(4);
            this.tags = new String[tagCount];
            for (int t = 0; t < tagCount; t++) {
                // Duplicate-friendly: tag values repeat across products
                this.tags[t] = new String(TAGS[rng.nextInt(TAGS.length)]); // runtime String!
            }

            int reviewCount = rng.nextInt(8);
            this.reviews = new ArrayList<>(reviewCount);
            for (int r = 0; r < reviewCount; r++) {
                reviews.add(new Review(
                    1 + rng.nextInt(5),
                    "user-" + rng.nextInt(100_000),
                    REVIEW_TEMPLATES[rng.nextInt(REVIEW_TEMPLATES.length)] + " (prod=" + id + ")"
                ));
            }
        }
    }

    static final String[] CATEGORIES = {
        "Electronics","Clothing","Books","Sports","Furniture",
        "Toys","Beauty","Automotive","Garden","Kitchen"
    };
    static final String[] TAGS = {
        "sale","new","featured","trending","bestseller",
        "limited","exclusive","premium","organic","eco-friendly"
    };
    static final String[] REVIEW_TEMPLATES = {
        "Great product, exactly as described.",
        "Good value for money, would recommend.",
        "Average quality, nothing special.",
        "Disappointed with the build quality.",
        "Exceeded my expectations, very happy."
    };

    // The in-memory cache — a single static root retaining everything
    static Map<Integer, Product> CATALOG;

    public static void main(String[] args) throws Exception {
        int N = 80_000;
        Random rng = new Random(42);

        System.out.println("Building in-memory product catalog with " + N + " products …");
        System.out.printf("  Each product: ~%d B description + tags + %d avg reviews%n",
            500, 3);

        CATALOG = new HashMap<>(N * 2);
        for (int i = 0; i < N; i++) {
            Product p = new Product(i, rng);
            CATALOG.put(p.id, p);
            if (i % 20_000 == 0) {
                Runtime rt = Runtime.getRuntime();
                System.out.printf("  %,d products … heap=%,d MB%n", i,
                    (rt.totalMemory() - rt.freeMemory()) >> 20);
            }
        }

        Runtime rt = Runtime.getRuntime();
        System.gc(); Thread.sleep(300);
        System.out.printf("%nCatalog ready: %,d products, heap=%,d MB / %,d MB%n",
            CATALOG.size(),
            (rt.totalMemory() - rt.freeMemory()) >> 20,
            rt.maxMemory() >> 20);
        System.out.println();
        System.out.println("Take a heap dump now:");
        System.out.println("  jcmd $(pgrep -f NormalDomainObjectGraph) GC.heap_dump heap.hprof");
        System.out.println();
        System.out.println("Analysis commands:");
        System.out.println("  hprof-analyzer heap summary heap.hprof");
        System.out.println("  hprof-analyzer heap histogram heap.hprof");
        System.out.println("  hprof-analyzer heap.hprof report.html --find-duplicates --collections");

        Thread.sleep(Long.MAX_VALUE);
    }
}

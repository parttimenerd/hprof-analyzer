/**
 * NORMAL APP 34 — E-commerce product catalog with search and price index
 *
 * A realistic domain object graph: products, categories, reviews, and two
 * secondary indexes (price range, keyword search). No leaks — all structures
 * are bounded to a fixed catalog size. Use it to explore how a mid-size domain
 * model looks on the heap.
 *
 * Run:   java -Xmx256m 34_AppProductCatalog.java
 *
 * Capture (after "HEAP_DUMP_READY" is printed):
 *   jcmd $(pgrep -f AppProductCatalog) GC.heap_dump heap.hprof
 *
 * What's in the heap:
 *   8 000 Product objects       → id, name (30 chars avg), description (200 chars),
 *                                  price, category, tags[], attributes Map
 *   8 000 Review[] arrays       → 0–10 reviews per product (Rating + text)
 *   CategoryNode tree           → depth-3 tree, 50 leaf categories
 *   Price index                 → TreeMap<Long, List<Product>> (price-centile buckets)
 *   Keyword search index        → HashMap<String, Set<Integer>> (word → product IDs)
 *   ~80 000 index String keys   → stems from product names + descriptions
 *
 * Interesting analysis:
 *   hprof-analyzer heap histogram heap.hprof
 *   → Product, Review, String dominate; CategoryNode much smaller
 *
 *   hprof-analyzer heap report heap.hprof --section leaks
 *   → Nothing — catalog is a fixed bounded structure
 *
 *   hprof-analyzer heap query heap.hprof \
 *       --oql "SELECT p.name, p.priceCents \
 *               FROM _34_AppProductCatalog\$Product p \
 *               ORDER BY p.priceCents DESC LIMIT 5"
 *
 *   hprof-analyzer heap.hprof report.html --find-duplicates
 *   → Many duplicate attribute-key strings across products (e.g. "brand", "color")
 *
 *   hprof-analyzer heap.hprof report.html --collections
 *   → TreeMap (price index), HashMap (keyword index) fill ratios
 */
import java.util.*;

public class _34_AppProductCatalog {

    // ── Domain model ──────────────────────────────────────────────────────
    static class Review {
        final int    rating;        // 1–5
        final String author;
        final String text;          // ~80 chars

        Review(int rating, String author, String text) {
            this.rating = rating;
            this.author = author;
            this.text   = text;
        }
    }

    static class Product {
        final int               id;
        final String            name;           // ~30 chars
        final String            description;    // ~200 chars
        final long              priceCents;
        final String            category;
        final String[]          tags;           // 2–5 tags
        final Map<String,String> attributes;   // brand, color, size, weight …
        final List<Review>      reviews;

        Product(int id, String name, String desc, long price, String cat,
                String[] tags, Map<String,String> attrs, List<Review> reviews) {
            this.id          = id;
            this.name        = name;
            this.description = desc;
            this.priceCents  = price;
            this.category    = cat;
            this.tags        = tags;
            this.attributes  = attrs;
            this.reviews     = reviews;
        }
    }

    static class CategoryNode {
        final String            name;
        final List<CategoryNode> children = new ArrayList<>();
        final List<Integer>     productIds = new ArrayList<>();

        CategoryNode(String name) { this.name = name; }
    }

    // ── Catalog state ─────────────────────────────────────────────────────
    static final List<Product>                    CATALOG      = new ArrayList<>(8_000);
    static CategoryNode                           CATEGORY_ROOT;
    static final TreeMap<Long, List<Integer>>     PRICE_INDEX  = new TreeMap<>();
    static final Map<String, Set<Integer>>        KEYWORD_IDX  = new HashMap<>();

    // ── Generation helpers ────────────────────────────────────────────────
    static final String[] ADJ  = {"premium","compact","ultra","smart","eco",
                                   "portable","wireless","digital","classic","pro"};
    static final String[] NOUN = {"device","kit","module","pack","system","unit",
                                   "set","bundle","hub","adapter","sensor","tool"};
    static final String[] BRANDS = {"Acme","Bravo","CoreTech","DeltaX","EcoSphere",
                                     "FlowTech","GridCore","HyperBase","InnoTech","JetLabs"};
    static final String[] COLORS = {"black","white","silver","blue","red","green","gray"};
    static final String[] SIZES  = {"XS","S","M","L","XL","2XL","One-size"};
    static final String[] CAT_L1 = {"Electronics","Home","Sports","Books","Tools",
                                     "Clothing","Health","Toys","Garden","Automotive"};
    static final String[] CAT_L2 = {"Accessories","Essentials","Premium","Budget","New-Arrivals"};

    static CategoryNode buildCategoryTree() {
        CategoryNode root = new CategoryNode("root");
        for (String l1 : CAT_L1) {
            CategoryNode n1 = new CategoryNode(l1);
            for (String l2 : CAT_L2) {
                n1.children.add(new CategoryNode(l1 + "/" + l2));
            }
            root.children.add(n1);
        }
        return root;
    }

    static String buildDescription(String name, String brand, String cat, Random rng) {
        String[] sentences = {
            "High-quality " + name.toLowerCase() + " from " + brand + ".",
            "Designed for " + CAT_L1[rng.nextInt(CAT_L1.length)].toLowerCase() + " enthusiasts.",
            "Compact design with premium materials ensures long-lasting durability.",
            "Compatible with all standard accessories and easy to install.",
            "Backed by a 2-year manufacturer warranty and 24/7 support."
        };
        StringBuilder sb = new StringBuilder(200);
        int n = 2 + rng.nextInt(3);
        for (int i = 0; i < n && i < sentences.length; i++) sb.append(sentences[i]).append(' ');
        return sb.toString().trim();
    }

    static Product generateProduct(int id, Random rng) {
        String adj   = ADJ[rng.nextInt(ADJ.length)];
        String noun  = NOUN[rng.nextInt(NOUN.length)];
        String name  = adj.substring(0,1).toUpperCase() + adj.substring(1)
                       + " " + noun.substring(0,1).toUpperCase() + noun.substring(1)
                       + " " + (1000 + id);
        String brand = BRANDS[rng.nextInt(BRANDS.length)];
        String cat   = CAT_L1[rng.nextInt(CAT_L1.length)] + "/"
                       + CAT_L2[rng.nextInt(CAT_L2.length)];
        long price   = 99L + (rng.nextInt(999_00) / 100) * 100L; // 99c – $999.99

        // Tags
        int tagCount = 2 + rng.nextInt(4);
        String[] tags = new String[tagCount];
        for (int i = 0; i < tagCount; i++)
            tags[i] = (i == 0 ? brand.toLowerCase() : ADJ[rng.nextInt(ADJ.length)]);

        // Attributes (the SAME keys repeated across most products → duplicate strings)
        Map<String, String> attrs = new HashMap<>(6);
        attrs.put("brand",  brand);
        attrs.put("color",  COLORS[rng.nextInt(COLORS.length)]);
        attrs.put("size",   SIZES[rng.nextInt(SIZES.length)]);
        attrs.put("weight", (100 + rng.nextInt(4900)) + "g");
        if (rng.nextBoolean()) attrs.put("material", "aluminum");
        if (rng.nextBoolean()) attrs.put("origin",   "EU");

        // Reviews (0–8 per product)
        int reviewCount = rng.nextInt(9);
        List<Review> reviews = new ArrayList<>(reviewCount);
        for (int r = 0; r < reviewCount; r++) {
            reviews.add(new Review(
                1 + rng.nextInt(5),
                "user" + rng.nextInt(50_000),
                "Review text number " + r + ": quality is "
                    + (rng.nextBoolean() ? "excellent" : "acceptable") + "."));
        }

        return new Product(id, name, buildDescription(name, brand, cat, rng),
                           price, cat, tags, attrs, reviews);
    }

    static void buildIndexes() {
        for (Product p : CATALOG) {
            // Price index (bucket by $10 band)
            long bucket = (p.priceCents / 1000) * 1000;
            PRICE_INDEX.computeIfAbsent(bucket, k -> new ArrayList<>()).add(p.id);

            // Keyword index from name + first 3 words of description
            Set<String> words = new HashSet<>();
            Collections.addAll(words, p.name.toLowerCase().split("\\W+"));
            String[] descWords = p.description.split("\\W+");
            for (int i = 0; i < Math.min(6, descWords.length); i++)
                words.add(descWords[i].toLowerCase());
            // Add tags
            for (String t : p.tags) words.add(t.toLowerCase());

            for (String w : words) {
                if (w.length() >= 3)
                    KEYWORD_IDX.computeIfAbsent(w, k -> new HashSet<>()).add(p.id);
            }

            // Assign to category tree
            for (CategoryNode n1 : CATEGORY_ROOT.children) {
                if (p.category.startsWith(n1.name)) {
                    for (CategoryNode n2 : n1.children) {
                        if (p.category.equals(n2.name)) { n2.productIds.add(p.id); break; }
                    }
                    break;
                }
            }
        }
    }

    public static void main(String[] args) throws Exception {
        System.out.println("=== E-Commerce Product Catalog ===");
        System.out.println("Building category tree …");
        CATEGORY_ROOT = buildCategoryTree();

        System.out.println("Generating 8 000 products …");
        Random rng = new Random(1234);
        for (int i = 0; i < 8_000; i++) CATALOG.add(generateProduct(i, rng));

        System.out.println("Building price + keyword indexes …");
        buildIndexes();

        Runtime rt = Runtime.getRuntime();
        System.gc(); Thread.sleep(200);
        long reviews = CATALOG.stream().mapToLong(p -> p.reviews.size()).sum();
        System.out.printf("Ready | heap %,d MB / %,d MB%n",
            (rt.totalMemory() - rt.freeMemory()) >> 20,
            rt.maxMemory() >> 20);
        System.out.printf("  products:       %,6d%n",  CATALOG.size());
        System.out.printf("  reviews total:  %,6d%n",  reviews);
        System.out.printf("  price buckets:  %,6d%n",  PRICE_INDEX.size());
        System.out.printf("  keyword terms:  %,6d%n",  KEYWORD_IDX.size());
        System.out.printf("  category nodes: %,6d%n",
            CATEGORY_ROOT.children.stream()
                .mapToInt(n -> 1 + n.children.size()).sum());
        System.out.println();
        System.out.println("HEAP_DUMP_READY");
        System.out.println("  jcmd $(pgrep -f AppProductCatalog) GC.heap_dump heap.hprof");
        Thread.sleep(Long.MAX_VALUE);
    }
}

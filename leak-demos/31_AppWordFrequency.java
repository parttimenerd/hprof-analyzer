/**
 * NORMAL APP 31 — Word-frequency corpus analyser
 *
 * A realistic text-processing application. No leaks or bugs — just a typical
 * in-memory representation of a processed document corpus. Use it to practice
 * answering: "Where does all this memory actually go?"
 *
 * Run:   java -Xmx256m 31_AppWordFrequency.java
 *
 * Capture (after "HEAP_DUMP_READY" is printed):
 *   jcmd $(pgrep -f AppWordFrequency) GC.heap_dump heap.hprof
 *
 * What's in the heap:
 *   ~3 000 Document objects              → doc id, title, word list
 *   ~150 000–200 000 unique String keys  → vocabulary
 *   HashMap<String, Integer>             → global term frequency
 *   HashMap<String, List<Integer>>       → inverted index (term → doc ids)
 *   HashMap<String, Map<String, Double>> → per-doc TF-IDF weights
 *   List<Map.Entry<String,Integer>>      → top-500 words sorted by freq
 *
 * Interesting analysis:
 *   hprof-analyzer heap histogram heap.hprof
 *   → String[] dominates; look at char[] (Java <9) or byte[] (compact strings)
 *
 *   hprof-analyzer heap query heap.hprof \
 *       --oql "SELECT COUNT(*) FROM java.lang.String"
 *
 *   hprof-analyzer heap.hprof report.html --find-duplicates
 *   → Many duplicate strings: common words stored per-doc AND in global map
 *
 *   hprof-analyzer heap query heap.hprof \
 *       --oql "SELECT @displayName, COUNT(*) AS n \
 *               FROM INSTANCEOF java.util.Map \
 *               GROUP BY @displayName ORDER BY n DESC LIMIT 10"
 *   → Shows HashMap vs LinkedHashMap count breakdown
 */
import java.util.*;
import java.util.stream.*;

public class _31_AppWordFrequency {

    // ── Domain model ──────────────────────────────────────────────────────
    static class Document {
        final int    id;
        final String title;
        final List<String> words;   // ALL words (with repetition)

        Document(int id, String title, List<String> words) {
            this.id    = id;
            this.title = title;
            this.words = words;
        }
    }

    // ── Corpus state ──────────────────────────────────────────────────────
    static final List<Document>                       CORPUS    = new ArrayList<>();
    static final Map<String, Integer>                 TERM_FREQ = new HashMap<>();
    static final Map<String, List<Integer>>           INV_INDEX = new HashMap<>();
    static final Map<Integer, Map<String, Double>>    TF_IDF    = new HashMap<>();
    static List<Map.Entry<String, Integer>>           TOP_WORDS;

    // ── Vocabulary: 3 000 unique words drawn from common English patterns ─
    static final String[] VOCAB;
    static {
        String[] prefixes = {"anti","auto","bio","co","de","dis","en","ex","geo",
                             "hyper","in","inter","macro","micro","mis","mono",
                             "multi","non","over","poly","post","pre","pro","re",
                             "semi","sub","super","trans","ultra","un","under"};
        String[] roots    = {"action","base","block","chain","class","code","data",
                             "event","field","flow","graph","group","handler","index",
                             "kernel","layer","list","lock","log","map","node","path",
                             "pool","queue","record","ref","request","result","rule",
                             "scope","service","set","sink","source","state","store",
                             "stream","table","task","token","tree","type","view",
                             "worker","zone","agent","batch","cache","client","config"};
        String[] suffixes = {"","able","al","ary","ate","ed","er","ful","ic","ing",
                             "ion","ish","ism","ist","ity","ive","ize","less","ly",
                             "ment","ness","or","ous","tion","ure","ward","wise"};
        Set<String> vocab = new LinkedHashSet<>();
        for (String p : prefixes) for (String r : roots)
            for (String s : suffixes) {
                vocab.add(p + r + s);
                if (vocab.size() >= 3_000) break;
            }
        VOCAB = vocab.toArray(new String[0]);
    }

    // ── Corpus generation ─────────────────────────────────────────────────
    static List<String> generateDocWords(Random rng, int count) {
        List<String> words = new ArrayList<>(count);
        // 80% of words from top-200 vocabulary (Zipf-like distribution)
        for (int i = 0; i < count; i++) {
            int idx = rng.nextDouble() < 0.8
                ? rng.nextInt(200)
                : rng.nextInt(VOCAB.length);
            words.add(VOCAB[idx]);
        }
        return words;
    }

    static void buildCorpus(int docCount) {
        Random rng = new Random(42);
        for (int d = 0; d < docCount; d++) {
            int wordCount = 150 + rng.nextInt(200);
            String title  = "document-" + d + "-" + VOCAB[rng.nextInt(100)];
            CORPUS.add(new Document(d, title, generateDocWords(rng, wordCount)));
        }
    }

    static void buildIndexes() {
        // Global term frequency + inverted index
        for (Document doc : CORPUS) {
            Set<String> seen = new HashSet<>();
            for (String word : doc.words) {
                TERM_FREQ.merge(word, 1, Integer::sum);
                if (seen.add(word)) {
                    INV_INDEX.computeIfAbsent(word, k -> new ArrayList<>()).add(doc.id);
                }
            }
        }

        // Per-document TF-IDF (simplified)
        int totalDocs = CORPUS.size();
        for (Document doc : CORPUS) {
            Map<String, Long> tf = doc.words.stream()
                .collect(Collectors.groupingBy(w -> w, Collectors.counting()));
            Map<String, Double> tfidf = new HashMap<>(tf.size());
            for (Map.Entry<String, Long> e : tf.entrySet()) {
                double termFreq = (double) e.getValue() / doc.words.size();
                int    df       = INV_INDEX.getOrDefault(e.getKey(), List.of()).size();
                double idf      = Math.log((1.0 + totalDocs) / (1.0 + df));
                tfidf.put(e.getKey(), termFreq * idf);
            }
            TF_IDF.put(doc.id, tfidf);
        }

        // Top 500 words by global frequency
        TOP_WORDS = TERM_FREQ.entrySet().stream()
            .sorted(Map.Entry.<String, Integer>comparingByValue().reversed())
            .limit(500)
            .collect(Collectors.toList());
    }

    public static void main(String[] args) throws Exception {
        System.out.println("=== Word-Frequency Corpus Analyser ===");
        System.out.println("Building corpus of 3 000 documents …");
        long t0 = System.currentTimeMillis();
        buildCorpus(3_000);

        System.out.println("Building indexes (term freq, inverted index, TF-IDF) …");
        buildIndexes();

        long elapsed = System.currentTimeMillis() - t0;
        Runtime rt = Runtime.getRuntime();
        System.gc(); Thread.sleep(200);
        System.out.printf("Ready in %,d ms | heap %,d MB / %,d MB%n",
            elapsed,
            (rt.totalMemory() - rt.freeMemory()) >> 20,
            rt.maxMemory() >> 20);
        System.out.printf("  corpus:        %,6d documents%n",  CORPUS.size());
        System.out.printf("  vocabulary:    %,6d terms%n",      TERM_FREQ.size());
        System.out.printf("  inverted idx:  %,6d entries%n",    INV_INDEX.size());
        System.out.printf("  TF-IDF docs:   %,6d%n",           TF_IDF.size());
        System.out.printf("  top word:      %s (%,d occurrences)%n",
            TOP_WORDS.get(0).getKey(), TOP_WORDS.get(0).getValue());
        System.out.println();
        System.out.println("HEAP_DUMP_READY");
        System.out.println("  jcmd $(pgrep -f AppWordFrequency) GC.heap_dump heap.hprof");
        Thread.sleep(Long.MAX_VALUE);
    }
}

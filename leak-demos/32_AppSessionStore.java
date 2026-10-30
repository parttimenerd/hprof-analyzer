/**
 * NORMAL APP 32 — Web-application session store
 *
 * A realistic simulation of a session store in a stateful web application.
 * No leaks — sessions are bounded by MAX_SESSIONS; old ones are evicted.
 * Use it to explore what a "healthy" session store looks like in a heap dump.
 *
 * Run:   java -Xmx256m 32_AppSessionStore.java
 *
 * Capture (after "HEAP_DUMP_READY" is printed):
 *   jcmd $(pgrep -f AppSessionStore) GC.heap_dump heap.hprof
 *
 * What's in the heap:
 *   10 000 Session objects       → id (UUID string), userId, roles[], createdAt
 *   10 000 UserProfile objects   → name, email, prefs Map, recent-actions List
 *   Session index by sessionId   → HashMap<String, Session>
 *   Session index by userId      → HashMap<Integer, Session>
 *   LRU eviction queue           → LinkedList of sessionIds
 *   Aggregate counters           → Map<String, AtomicLong> by role, by region
 *
 * Interesting analysis:
 *   hprof-analyzer heap histogram heap.hprof
 *   → String[] and char[]/byte[] dominate due to UUID session IDs
 *
 *   hprof-analyzer heap query heap.hprof \
 *       --oql "SELECT COUNT(*) FROM _32_AppSessionStore\$Session"
 *
 *   hprof-analyzer heap query heap.hprof \
 *       --oql "SELECT s.sessionId, s.profile.email \
 *               FROM _32_AppSessionStore\$Session s LIMIT 5"
 *
 *   hprof-analyzer heap report heap.hprof --section leaks
 *   → Nothing — the store is correctly bounded
 *
 *   hprof-analyzer heap.hprof report.html --full-analysis
 *   → Collections section shows LinkedList (LRU queue) and HashMap stats
 */
import java.util.*;
import java.util.concurrent.atomic.*;

public class _32_AppSessionStore {

    static final int MAX_SESSIONS = 10_000;

    // ── Domain objects ────────────────────────────────────────────────────
    static class UserProfile {
        final int    userId;
        final String name;
        final String email;
        final String region;                        // "EU", "US", "APAC"
        final Map<String, String> preferences;      // theme, lang, timezone …
        final List<String>        recentActions;    // last 20 action IDs

        UserProfile(int id, Random rng) {
            this.userId  = id;
            this.name    = "User-" + id;
            this.email   = "user" + id + "@example.com";
            this.region  = REGIONS[rng.nextInt(REGIONS.length)];
            this.preferences = new HashMap<>(6);
            preferences.put("theme",    THEMES[rng.nextInt(THEMES.length)]);
            preferences.put("lang",     LANGS[rng.nextInt(LANGS.length)]);
            preferences.put("timezone", "UTC+" + rng.nextInt(12));
            preferences.put("pageSize", String.valueOf(10 + rng.nextInt(90)));
            this.recentActions = new ArrayList<>(20);
            for (int i = 0; i < 20; i++)
                recentActions.add("action-" + rng.nextInt(100_000));
        }
    }

    static class Session {
        final String      sessionId;    // UUID-like string
        final int         userId;
        final String[]    roles;
        final long        createdAt;
        long              lastActiveAt;
        final UserProfile profile;

        Session(int userId, UserProfile profile, Random rng) {
            this.sessionId    = uuid(userId, rng);
            this.userId       = userId;
            this.roles        = pickRoles(rng);
            this.createdAt    = System.currentTimeMillis() - rng.nextInt(86_400_000);
            this.lastActiveAt = this.createdAt + rng.nextInt(3_600_000);
            this.profile      = profile;
        }
    }

    // ── Store state ───────────────────────────────────────────────────────
    static final Map<String, Session>          BY_SESSION_ID = new LinkedHashMap<>();
    static final Map<Integer, Session>         BY_USER_ID    = new HashMap<>();
    static final Deque<String>                 LRU_QUEUE     = new ArrayDeque<>();
    static final Map<String, AtomicLong>       ROLE_COUNTS   = new HashMap<>();
    static final Map<String, AtomicLong>       REGION_COUNTS = new HashMap<>();

    // ── Helpers ───────────────────────────────────────────────────────────
    static final String[] REGIONS = {"EU", "US", "APAC", "LATAM", "MEA"};
    static final String[] THEMES  = {"light", "dark", "high-contrast"};
    static final String[] LANGS   = {"en", "de", "fr", "ja", "zh", "es", "pt"};
    static final String[] ALL_ROLES = {"admin", "editor", "viewer", "api", "billing"};

    static String uuid(int seed, Random rng) {
        return String.format("%08x-%04x-%04x-%04x-%012x",
            seed, rng.nextInt(0xFFFF), rng.nextInt(0xFFFF),
            rng.nextInt(0xFFFF), (long)(rng.nextInt()) & 0xFFFFFFFFFFFFL);
    }

    static String[] pickRoles(Random rng) {
        int n = 1 + rng.nextInt(3);
        Set<String> picked = new LinkedHashSet<>();
        while (picked.size() < n) picked.add(ALL_ROLES[rng.nextInt(ALL_ROLES.length)]);
        return picked.toArray(new String[0]);
    }

    static void addSession(Session s) {
        // LRU eviction
        if (BY_SESSION_ID.size() >= MAX_SESSIONS) {
            String oldest = LRU_QUEUE.poll();
            if (oldest != null) {
                Session evicted = BY_SESSION_ID.remove(oldest);
                if (evicted != null) BY_USER_ID.remove(evicted.userId);
            }
        }
        BY_SESSION_ID.put(s.sessionId, s);
        BY_USER_ID.put(s.userId, s);
        LRU_QUEUE.add(s.sessionId);
        for (String role : s.roles)
            ROLE_COUNTS.computeIfAbsent(role, k -> new AtomicLong()).incrementAndGet();
        REGION_COUNTS.computeIfAbsent(s.profile.region, k -> new AtomicLong()).incrementAndGet();
    }

    public static void main(String[] args) throws Exception {
        System.out.println("=== Web Application Session Store ===");
        System.out.println("Populating " + MAX_SESSIONS + " sessions …");

        Random rng = new Random(99);
        for (int i = 0; i < MAX_SESSIONS; i++) {
            UserProfile profile = new UserProfile(i, rng);
            addSession(new Session(i, profile, rng));
        }

        Runtime rt = Runtime.getRuntime();
        System.gc(); Thread.sleep(200);
        System.out.printf("Ready | heap %,d MB / %,d MB%n",
            (rt.totalMemory() - rt.freeMemory()) >> 20,
            rt.maxMemory() >> 20);
        System.out.printf("  sessions:   %,6d%n", BY_SESSION_ID.size());
        System.out.printf("  lru-queue:  %,6d%n", LRU_QUEUE.size());
        System.out.printf("  role dist:  %s%n",   ROLE_COUNTS);
        System.out.printf("  regions:    %s%n",   REGION_COUNTS);
        System.out.println();
        System.out.println("HEAP_DUMP_READY");
        System.out.println("  jcmd $(pgrep -f AppSessionStore) GC.heap_dump heap.hprof");
        Thread.sleep(Long.MAX_VALUE);
    }
}

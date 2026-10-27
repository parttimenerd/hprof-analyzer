# Spring Leak Clinic

A self-contained Spring Boot application with **7 intentional memory problems**,
built as a ready-to-run demo for
[hprof-analyzer](https://github.com/parttimenerd/hprof-analyzer) talks and
workshops. Each leak is designed to light up a different section of the
hprof-analyzer report so you can walk an audience through the full analysis
workflow in a single session.

The app looks like a plausible microservice — REST endpoints, session handling,
an event bus, a report generator — but every component harbours a common
production-grade memory problem. The patterns are drawn from real incidents: the
kind of bugs that only surface under sustained load, survive code review because
they look reasonable, and end up on the OOM-killed-at-3-AM pager.

## Quick start

```bash
cd demo/spring-leak-clinic

# One command: build → start → trigger leaks → heap dump → full analysis
bash run-and-dump.sh

# Open the HTML report in your browser
open leak-clinic-report.html
```

**Prerequisites:**
- Java 17+ (`java` and `jcmd` on PATH)
- Maven 3.8+ (`mvn` on PATH)
- `hprof-analyzer` on PATH *(optional — the script captures the dump either way)*

**Script flags:**
| Flag | Default | Description |
|------|---------|-------------|
| `--rounds N` | `100` | Number of warm-up rounds per leak (more rounds = bigger heap) |
| `--xmx SIZE` | `512m` | Max heap size for the JVM |
| `--skip-build` | off | Reuse an already-built JAR in `target/` |

---

## The 7 leaks at a glance

| # | Source file | Pattern | Primary report section |
|---|------------|---------|------------------------|
| 1 | [`EventLog.java`](#leak-1-unbounded-event-log) | Unbounded static `ArrayList<byte[]>` | Leak Suspects, Top Consumers |
| 2 | [`SessionStore.java`](#leak-2-session-cache-without-eviction) | `ConcurrentHashMap` cache, no TTL/eviction | Leak Suspects, Top Consumers |
| 3 | [`RequestTracker.java`](#leak-3-threadlocal-never-cleared) | `ThreadLocal` never `.remove()`d in a thread pool | Threads |
| 4 | [`NotificationHub.java`](#leak-4-listenercallback-leak) | Listener lambdas capturing large context, never unregistered | Leak Suspects |
| 5 | [`ReportEngine.java`](#leak-5-duplicate-strings) | Thousands of non-interned `String` copies | Duplicate Strings |
| 6 | [`ReportEngine.java`](#leak-6-oversized-empty-collections) | `HashMap(10_000)` / `ArrayList(10_000)` with 1 entry | Collections |
| 7 | [`application.properties`](#leak-7-secrets-on-the-heap) | API keys, DB passwords, JWT secrets in config | Secret Detection |

---

## Leak-by-leak walkthrough

### Leak 1: Unbounded event log

**File:** `EventLog.java` — 39 lines

**The pattern.** A static `ArrayList` acts as an in-memory audit log. Every
incoming request appends a 50 KB serialised event payload. The list is
synchronised, so it is thread-safe, and the code looks professional. What is
missing is any form of size cap, rotation, or expiry. The list grows
monotonically until the JVM runs out of heap.

```java
// BUG: static list, never cleared
private static final List<byte[]> EVENTS =
        Collections.synchronizedList(new ArrayList<>());

public void log(String source) {
    byte[] payload = new byte[50_000];   // ~50 KB per event
    EVENTS.add(payload);
}
```

**Where you have seen this before.** In-memory metrics collectors, request
replay buffers, undo history stacks, CDC event queues that never drain when the
downstream consumer is slower than the producer.

**What hprof-analyzer shows.**
- **Leak Suspects** — top suspect. The GC root chain reads:
  `GC Root (class) → Class EventLog → EVENTS (ArrayList) → elementData (Object[]) → byte[]`.
  The "accumulated objects" callout shows that the `byte[]` instances are the
  retained bulk.
- **Top Consumers** — `byte[]` dominates the class-level retained-size ranking
  because it is the payload type for leaks 1, 2, and 4.

**The fix.** Bound the list (ring buffer, `EvictingQueue`, or a proper
time-series store), or offload to an external system (Kafka, a log file).

---

### Leak 2: Session cache without eviction

**File:** `SessionStore.java` — 56 lines

**The pattern.** A `ConcurrentHashMap` stores user sessions. Each session
contains the user's profile metadata *and* a 20 KB simulated avatar image as a
`byte[]`. Sessions are created on login but never removed — there is no logout
handler and no TTL-based eviction. The map keys are user IDs, so load testing
with unique synthetic user IDs grows the map without bound.

```java
// BUG: sessions are added but never removed or expired
private static final Map<String, UserSession> SESSIONS = new ConcurrentHashMap<>();

public void login(String userId) {
    SESSIONS.put(userId, new UserSession(userId));
}
```

Each `UserSession` also uses `new String("ROLE_USER")` and `new String("dark")`
for its fields — deliberately defeating interning so these duplicates show up
in the duplicate-strings report (feeding Leak 5).

**Where you have seen this before.** Servlet-based session stores without an
idle timeout, API-gateway rate-limit caches keyed by IP that never expire,
connection pools that keep metadata for every client that ever connected.

**What hprof-analyzer shows.**
- **Leak Suspects** — the `ConcurrentHashMap` retaining thousands of
  `UserSession` instances, each pointing to `byte[] profileImage`.
- **Top Consumers** — `SessionStore$UserSession` appears high in the
  by-class retained ranking.
- **Collections** (with `--collections`) — the per-session `preferences` map
  has 3 entries each, perfectly sized, but there are thousands of them;
  this exercises the "many small maps" dimension of the report.

**The fix.** Add a TTL (Caffeine / Guava cache with `expireAfterAccess`),
cap the map size, or use a proper session store (Redis, Spring Session).

---

### Leak 3: ThreadLocal never cleared

**File:** `RequestTracker.java` — 43 lines

**The pattern.** A `ThreadLocal<List<String>>` accumulates distributed-tracing
context for the current request. Each entry is about 500 characters of "trace
metadata". The `ThreadLocal` is initialised lazily per thread but `.remove()` is
never called. Because Spring Boot uses a thread pool (`http-nio-*-exec-*`),
threads are *reused*, not destroyed. Each new request on the same thread appends
more entries without clearing the old ones.

```java
// BUG: never calls .remove()
private static final ThreadLocal<List<String>> CONTEXT =
        ThreadLocal.withInitial(ArrayList::new);

public void track(String requestId) {
    List<String> ctx = CONTEXT.get();
    ctx.add("trace-id=" + requestId + " baggage=" + "x".repeat(400));
    // Missing: CONTEXT.remove() in a finally block!
}
```

**Where you have seen this before.** MDC-style logging contexts, per-request
security principals, transaction managers that store the current TX in a
ThreadLocal. Any framework that sets up a ThreadLocal "request scope" and
relies on the caller to clean up.

**This is also a security bug.** Request data from user A bleeds into the
thread that later serves user B. In a real system this could leak auth tokens,
tenant IDs, or PII across request boundaries.

**What hprof-analyzer shows.**
- **Threads** — each `http-nio-18080-exec-*` worker thread shows an
  `ArrayList<String>` in its local-variable / ThreadLocalMap section, growing
  with every request the thread served.
- The retention path is:
  `Thread → threadLocals (ThreadLocalMap) → Entry[] → Entry.value (ArrayList) → String[]`.
- **Leak Suspects** may also flag this if the ThreadLocal data becomes one of
  the top retained subgraphs.

**The fix.** Always call `CONTEXT.remove()` in a `finally` block (or a servlet
`Filter` / Spring `HandlerInterceptor` afterCompletion). Better: use
`ScopedValue` (JEP 429, Java 21+) which is inherently request-scoped and
cannot leak across boundaries.

---

### Leak 4: Listener / callback leak

**File:** `NotificationHub.java` — 48 lines

**The pattern.** A publish-subscribe notification hub. Each "subscriber" is a
lambda that captures a 100 KB `byte[]` context. Listeners are added to a
static list on every request but never removed. The lambda keeps a strong
reference to its captured `processingContext` array, so it cannot be
garbage-collected as long as the lambda is reachable from the list.

```java
// BUG: listeners added, never removed
private static final List<Consumer<String>> LISTENERS =
        Collections.synchronizedList(new ArrayList<>());

public void subscribe(String subscriberId) {
    byte[] processingContext = new byte[100_000];  // 100 KB, captured by lambda
    LISTENERS.add(event -> {
        int checksum = processingContext[0] + event.length();
    });
}
```

**Where you have seen this before.** Swing/JavaFX `addPropertyChangeListener`
without a matching `remove`, Spring `ApplicationEventMulticaster` when beans
are registered and never cleaned up, Observable/RxJava subscriptions that are
never disposed, `ScheduledExecutorService.scheduleAtFixedRate()` runnables
that capture their creating context.

**What hprof-analyzer shows.**
- **Leak Suspects** — the static `LISTENERS` list retaining lambda instances,
  each dominating a 100 KB `byte[]`. The report shows the lambda's synthetic
  class name (`NotificationHub$$Lambda$...`) and the captured field.
- **Top Consumers** — the generated lambda class and `byte[]` appear together.

**The fix.** Use weak references (`WeakReference<Consumer>`), or provide an
explicit `unsubscribe(id)` API, or use a lifecycle-aware pub-sub framework
(Spring's `@EventListener` with `@Scope("request")`).

---

### Leak 5: Duplicate strings

**File:** `ReportEngine.java` — lines 33–46

**The pattern.** Each generated "report" creates 200 copies of 10 different
string values using `new String(...)`, defeating the JVM's string interning
and deduplication. Over 150 report invocations this produces 30,000+ redundant
String objects with identical content — a realistic simulation of what happens
when an application deserialises JSON, XML, or database result sets without
explicit deduplication.

```java
for (int i = 0; i < 200; i++) {
    statuses.add(new String("ACTIVE"));
    statuses.add(new String("PENDING"));
    statuses.add(new String("COMPLETED"));
    statuses.add(new String("CANCELLED"));
    statuses.add(new String("ERROR"));
    statuses.add(new String("ROLE_USER"));
    statuses.add(new String("application/json"));
    statuses.add(new String("Content-Type"));
    statuses.add(new String("Authorization"));
    statuses.add(new String("Bearer"));
}
```

The `SessionStore.UserSession` also contributes: every session creates
`new String("ROLE_USER")`, `new String("dark")`, `new String("en_US")`,
and `new String("UTC")`.

**Where you have seen this before.** Jackson/Gson deserialisation of repeated
enum-like fields, Hibernate `ResultSet` mapping (every `row.getString("status")`
creates a fresh object), HTTP header parsing (the same `"Content-Type"` string
materialised for every request), Spring's property resolution.

**What hprof-analyzer shows.**
- **Duplicate Strings** (requires `--find-duplicates`) — the top offenders
  table lists `"ACTIVE"`, `"PENDING"`, `"COMPLETED"`, etc. with thousands of
  copies each, total wasted bytes, and which classes hold the most string
  references.

**The fix.** Enable `-XX:+UseStringDeduplication` (G1/ZGC), use
`String.intern()` or a manual dedup map for hot paths, or configure your JSON
library's field name cache (Jackson's `InternedStringPool`).

---

### Leak 6: Oversized empty collections

**File:** `ReportEngine.java` — lines 49–57

**The pattern.** Each report pre-allocates a `HashMap(10_000)` and an
`ArrayList(10_000)` but only puts a single entry into each. The internal
`Object[]` / `Node[]` backing arrays are fully allocated at construction
time, wasting ~40 KB per map and ~40 KB per list. Over 150 reports this adds
up to ~12 MB of wasted capacity.

```java
Map<String, Object> sparseCache = new HashMap<>(10_000);
sparseCache.put("report-" + reportId, statuses);        // 1 entry in a 10k map

List<Object> sparseBuffer = new ArrayList<>(10_000);
sparseBuffer.add(reportId);                              // 1 entry in a 10k list
```

**Where you have seen this before.** Copy-pasted initial capacities from a
different context ("this other map needed 10k, so I'll use 10k here too"),
pessimistic sizing in generated code (ORM query result lists pre-sized for
`maxResults`), deserialisation that preserves the serialised capacity.

**What hprof-analyzer shows.**
- **Collections** (requires `--collections`) — the fill-ratio column shows
  entries like `1 / 16384` (HashMap rounds up to the next power of 2) with
  a fill ratio near 0.00%.  The per-class attribution shows
  `ReportEngine.REPORTS → HashMap` and `ReportEngine.REPORTS → ArrayList`.

**The fix.** Use the default constructor (`new HashMap<>()`, `new ArrayList<>()`)
unless you have measured that the initial capacity matters. For known-small
maps, `Map.of()` or `Map.copyOf()` produce compact unmodifiable maps.

---

### Leak 7: Secrets on the heap

**File:** `application.properties`

**The pattern.** Spring Boot reads all properties into its `Environment`
abstraction, which stores values as `java.lang.String` objects on the heap. Any
secret placed in `application.properties` — database passwords, API keys, JWT
signing secrets — is visible in a heap dump as a plain-text string.

```properties
spring.datasource.password=s3cret_db_pass!
app.openai.api-key=sk-proj-demoFakeKey0123456789abcdef0123456789abcdefABCD
app.stripe.secret-key=sk_live_51Hx0000000000000000000000000000000000000000demo
app.internal.jwt-secret=eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJkZW1vIiwiZXhwIjo5OTk5OTk5OTk5fQ.fake
```

This is not a memory *leak* in the traditional sense (the memory does not
grow), but it is a critical security finding: heap dumps taken in production
contain credentials, and anyone with access to the dump file can extract them.

**Where you have seen this before.** Every Spring Boot application that stores
secrets in properties files, environment variables (also on the heap via
`System.getenv()`), or in-memory credential caches. Cloud provider SDKs
store access tokens in memory. JDBC drivers keep the connection password.
HTTP clients keep OAuth bearer tokens.

**What hprof-analyzer shows.**
- **Secret Detection** (`hprof-analyzer detect-secrets`) — finds the OpenAI
  `sk-proj-...` and Stripe `sk_live_...` patterns, the JWT token, and the
  database password.
- **OQL** — you can search manually:
  ```
  SELECT toString(s) FROM java.lang.String s WHERE toString(s) LIKE "sk-.*"
  ```

**The fix.** Use a vault (HashiCorp Vault, AWS Secrets Manager), rotate secrets
frequently, and use `char[]` instead of `String` for passwords when possible
(so you can zero the array after use). Redact heap dumps before sharing:
```bash
hprof-analyzer redact leak-clinic.hprof leak-clinic-redacted.hprof
```

---

## Exploring the report section by section

### Leak Suspects

Open `leak-clinic-report.html` → **Leak Suspects** tab.

The report groups objects into *accumulation points* — places where many
instances share a common path from a GC root. You should see three or four
suspects:

1. **Suspect 1** — `EventLog.EVENTS` (the unbounded static list). The
   GC root chain reads:
   `Class com.example.leakclinic.EventLog` → `EVENTS` → `elementData` → `byte[]`.
   The retained size is ~5 MB per 100 events.

2. **Suspect 2** — `NotificationHub.LISTENERS`. Each lambda instance retains
   its captured 100 KB `byte[] processingContext`. The synthetic class name
   (`NotificationHub$$Lambda$...`) confirms this is a lambda capture.

3. **Suspect 3** — `SessionStore.SESSIONS`. A `ConcurrentHashMap` retaining
   thousands of `UserSession` objects, each with a 20 KB avatar.

### Top Consumers

The **Top Consumers** tab ranks classes by *retained* size (not just shallow).
Key entries:

- `byte[]` at the top — the payload type for leaks 1, 2, and 4.
- `java.lang.String` — both the duplicate copies (leak 5) and the normal
  String usage by Spring.
- `java.lang.Object[]` — the backing arrays of the oversized collections
  (leak 6) plus the `ArrayList.elementData` arrays.
- `SessionStore$UserSession` and `NotificationHub$$Lambda$...` with per-class
  retained-size breakdowns.

### Threads

The **Threads** section shows each `http-nio-18080-exec-*` worker thread with:
- The thread's stack trace at the moment of the dump.
- The local variables and ThreadLocalMap entries the thread is retaining.

The ThreadLocal leak (leak 3) is visible here: each worker thread retains an
`ArrayList<String>` via `ThreadLocalMap$Entry`. The deeper you look, the more
entries you see per thread — each one a "trace-id=..." string from a request
that was served long ago but whose data is still pinned.

### Duplicate Strings

*(requires `--find-duplicates`)*

The duplicate-strings report lists:

| String value | Copies | Wasted bytes |
|-------------|--------|-------------|
| `"ACTIVE"` | 30,000+ | ~450 KB |
| `"PENDING"` | 30,000+ | ~500 KB |
| `"COMPLETED"` | 30,000+ | ~600 KB |
| `"CANCELLED"` | 30,000+ | ~600 KB |
| `"ERROR"` | 30,000+ | ~400 KB |
| `"ROLE_USER"` | hundreds | depends on sessions |
| `"Content-Type"` | 30,000+ | ~700 KB |
| `"application/json"` | 30,000+ | ~1 MB |

*(Exact counts depend on `--rounds`.)*

The report also shows which classes hold the most string references, helping
you trace the duplication back to `ReportEngine` and `SessionStore$UserSession`.

### Collections

*(requires `--collections`)*

The collections report shows:

- **Fill ratios** — `HashMap` instances with capacity 16,384 (rounded up from
  10,000) and size 1, showing a 0.01% fill ratio.
- **Size distributions** — hundreds of `HashMap` and `ArrayList` instances
  with oversized backing arrays.
- **Per-class attribution** — `ReportEngine.REPORTS` as the holder of the
  sparse collections.
- **Collision rates** — the single-entry `HashMap` instances have zero
  collisions (obviously), but a real-world report would show collision
  hot spots from bad `hashCode()` implementations.

### Secret Detection

```bash
hprof-analyzer detect-secrets leak-clinic.hprof
```

Expected findings:
- `sk-proj-demoFakeKey0123456789abcdef...` — matches the OpenAI API key pattern
- `sk_live_51Hx000...` — matches the Stripe secret key pattern
- `s3cret_db_pass!` — database password, found via the JDBC URL and the
  explicit `spring.datasource.password` property
- `eyJhbGciOiJIUzI1NiJ9...` — JWT token (base64-encoded JSON with `alg` header)

---

## OQL queries to try

```bash
# ── Leak 1: Count the unbounded event log entries ────────────────────────────
hprof-analyzer heap query leak-clinic.hprof \
  --oql "SELECT COUNT(*) FROM INSTANCEOF byte[] WHERE @length == 50000"

# ── Leak 2: Show all cached sessions ────────────────────────────────────────
hprof-analyzer heap query leak-clinic.hprof \
  --oql "SELECT s.userId, s.email, s.createdAt \
         FROM com.example.leakclinic.SessionStore\$UserSession s \
         LIMIT 20"

# ── Leak 3: Thread-retained memory per HTTP worker ──────────────────────────
hprof-analyzer heap query leak-clinic.hprof \
  --oql "SELECT t.@displayName, t.@retainedHeapSize \
         FROM java.lang.Thread t \
         WHERE t.@displayName LIKE \"http-nio-.*\" \
         ORDER BY t.@retainedHeapSize DESC"

# ── Leak 4: Count notification listeners ─────────────────────────────────────
hprof-analyzer heap query leak-clinic.hprof \
  --oql "SELECT COUNT(*) FROM java.util.function.Consumer"

# ── Leak 5: Find duplicate string values ─────────────────────────────────────
hprof-analyzer heap query leak-clinic.hprof \
  --oql "SELECT toString(s), COUNT(*) AS copies \
         FROM java.lang.String s \
         GROUP BY toString(s) \
         HAVING COUNT(*) > 100 \
         ORDER BY copies DESC \
         LIMIT 20"

# ── Leak 6: Find oversized collections ──────────────────────────────────────
hprof-analyzer heap query leak-clinic.hprof \
  --oql "SELECT h, h.size, h.table.@length AS capacity \
         FROM java.util.HashMap h \
         WHERE h.table.@length > 1000 \
         ORDER BY h.table.@length DESC \
         LIMIT 10"

# ── Leak 7: Hunt for API keys on the heap ───────────────────────────────────
hprof-analyzer heap query leak-clinic.hprof \
  --oql "SELECT toString(s) FROM java.lang.String s \
         WHERE toString(s) LIKE \"sk-.*\" LIMIT 10"

# ── Bonus: Retained size of the top 10 objects in the entire heap ────────────
hprof-analyzer heap query leak-clinic.hprof \
  --oql "SELECT @className, @retainedHeapSize \
         FROM INSTANCEOF java.lang.Object \
         ORDER BY @retainedHeapSize DESC \
         LIMIT 10"
```

---

## Suggested talk flow

### Opening (2 min)

> "This is a Spring Boot microservice. It has REST endpoints, session handling,
> an event bus, a report engine. It looks like the code you write every day.
> It has seven bugs. Let's find them."

### Act 1 — The headline leak (5 min)

1. Show `EventLog.java` on a slide (15 lines). Ask the audience: "See the bug?"
2. Run `bash run-and-dump.sh` (takes ~30 seconds live, or pre-record it).
3. Open `leak-clinic-report.html` → **Leak Suspects**.
4. Walk through Suspect 1: the GC root chain from `Class EventLog` → `EVENTS`
   → `byte[]`. Explain *retained size* vs *shallow size*.

### Act 2 — Beyond the obvious (5 min)

5. Switch to **Top Consumers**. Show that `byte[]` dominates — but it is
   coming from three different leaks (EventLog, SessionStore, NotificationHub).
6. Click into Suspect 2 (NotificationHub). Show that a *lambda* is retaining
   100 KB via its captured field. "Lambdas look free, but they capture."
7. Show Suspect 3 (SessionStore). "No logout handler, no TTL."

### Act 3 — The subtle leak (3 min)

8. Switch to **Threads**. Show the `http-nio-*-exec-*` threads.
9. Drill into one thread's ThreadLocalMap entries. Show the accumulated
   `ArrayList<String>` with request data from different users.
10. "This is also a security bug. User A's trace data is visible to user B."

### Act 4 — Death by a thousand cuts (3 min)

11. Switch to **Duplicate Strings**. Show 30,000 copies of `"ACTIVE"`.
12. "Every JSON deserialisation creates a new String. The JVM doesn't dedup
    by default."
13. Switch to **Collections**. Show the 0.01% fill-ratio HashMap.
14. "Someone copy-pasted `new HashMap(10_000)` from a different context."

### Act 5 — The security angle (2 min)

15. Run `hprof-analyzer detect-secrets leak-clinic.hprof`.
16. Show the API keys, JWT, DB password. "Heap dumps contain everything.
    Always redact before sharing."
17. Show `hprof-analyzer redact` as the fix.

### Closing (1 min)

18. Recap: 7 bugs, 5 report sections, 1 tool, 20 minutes.
19. Point to hprof-analyzer: CLI, browser UI, MCP server for AI-assisted
    triage.

---

## Manual workflow (without run-and-dump.sh)

```bash
# Build
mvn package -DskipTests

# Start
java -Xmx512m -jar target/spring-leak-clinic-1.0.0.jar &
APP_PID=$!

# Wait for startup
until curl -sf http://localhost:18080/api/health; do sleep 1; done

# Trigger all leaks (100 rounds)
curl -X POST "http://localhost:18080/api/warm-up?rounds=100"

# Extra ThreadLocal traffic across threads (parallel requests)
for i in $(seq 1 500); do
  curl -sf -X POST "http://localhost:18080/api/track?requestId=extra-$i" &
done
wait

# Capture heap dump
jcmd $APP_PID GC.heap_dump $PWD/leak-clinic.hprof

# Full analysis with all optional features
hprof-analyzer leak-clinic.hprof leak-clinic-report.html \
  --find-duplicates --collections

# Secret scan
hprof-analyzer detect-secrets leak-clinic.hprof

# Redact before sharing
hprof-analyzer redact leak-clinic.hprof leak-clinic-redacted.hprof

# Stop the app
kill $APP_PID
```

### Using the browser UI

```bash
hprof-analyzer server leak-clinic.hprof
# Open http://127.0.0.1:7070 in your browser
```

Or drop the `.hprof` file onto
[parttimenerd.github.io/hprof-analyzer](https://parttimenerd.github.io/hprof-analyzer/)
for in-browser analysis (no upload — everything runs locally via WebAssembly).

### Using the MCP server (AI-assisted triage)

```bash
# Register with Claude Code
claude mcp add hprof -- hprof-analyzer mcp

# Then in a conversation:
#   "Load leak-clinic.hprof and show me the leak suspects"
#   "What is the largest retained object?"
#   "Find any API keys on the heap"
```

---

## REST API reference

All endpoints are under `/api`. The `run-and-dump.sh` script calls them
automatically; these are listed for manual exploration or custom load scripts.

| Method | Endpoint | Leak | Description |
|--------|----------|------|-------------|
| `POST` | `/api/events?source=api` | 1 | Append a 50 KB event to the unbounded log |
| `POST` | `/api/sessions/{userId}` | 2 | Create a session with 20 KB profile image |
| `POST` | `/api/track?requestId=req` | 3 | Append to the ThreadLocal context list |
| `POST` | `/api/subscribe/{subscriberId}` | 4 | Register a listener capturing 100 KB context |
| `POST` | `/api/reports?id=r` | 5, 6 | Generate report with dup strings + oversized collections |
| `POST` | `/api/warm-up?rounds=100` | all | Trigger all leaks N times |
| `GET`  | `/api/health` | — | Health check (`{"status":"UP"}`) |

---

## File layout

```
spring-leak-clinic/
├── pom.xml                           Spring Boot 3.3, Java 17, spring-boot-starter-web
├── run-and-dump.sh                   Automated: build → run → trigger → dump → analyse
├── README.md                         This file
└── src/main/java/com/example/leakclinic/
    ├── LeakClinicApplication.java    @SpringBootApplication entry point
    ├── ApiController.java            REST endpoints — one per leak + warm-up
    ├── EventLog.java                 Leak 1: unbounded static ArrayList<byte[]>
    ├── SessionStore.java             Leak 2: ConcurrentHashMap session cache, no eviction
    ├── RequestTracker.java           Leak 3: ThreadLocal<List<String>> never cleared
    ├── NotificationHub.java          Leak 4: listener lambdas capturing 100 KB context
    └── ReportEngine.java             Leaks 5+6: duplicate strings + oversized collections
```

---

## Why these specific leaks?

The goal was to cover the **broadest surface of hprof-analyzer's report** with
the **fewest moving parts**. Each leak was chosen to be:

1. **Realistic** — drawn from actual production incidents, not contrived examples.
2. **Discoverable** — clearly visible in a specific report section without
   needing expert knowledge of the JVM internals.
3. **Slide-friendly** — each leak file is under 60 lines and can be shown
   on a single slide.
4. **Independent** — you can discuss any leak without having explained the
   others first. Mix and match for a 5-minute lightning talk or a 45-minute
   deep dive.
5. **Fixable** — every leak has a straightforward, well-known fix that you can
   explain in one sentence.

| Report section | Requires flag | Covered by leak(s) |
|---------------|---------------|---------------------|
| System Overview | — | all (heap size, class count) |
| Leak Suspects | — | 1, 2, 4 |
| Top Consumers | — | 1, 2, 4 |
| Threads | — | 3 |
| Duplicate Strings | `--find-duplicates` | 5 (+ minor contribution from 2) |
| Collections | `--collections` | 6 (+ minor contribution from 2) |
| Secret Detection | `detect-secrets` subcommand | 7 |

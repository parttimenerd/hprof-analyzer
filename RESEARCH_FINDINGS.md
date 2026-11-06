# Research Findings: Uncovered Views & Analyses for hprof-analyzer Users

> **Methodology**: Four parallel research streams covering (1) JVM tools beyond MAT, (2) non-JVM memory analysis ecosystems, (3) production guides & workflows, and (4) advanced/academic analysis views. All findings sourced from web searches (2023–2026). Excludes what's already in the hprof-analyzer skill (installation, OQL, dominator tree, leak suspects, thread analysis, secrets, redaction, collections, duplicate strings, large dumps, MAT caches, heap capture).

---

## Priority 1 — High-Impact, Directly Actionable

### 1. Framework-Specific Heap Patterns (Not Covered)

The skill covers generic leak patterns but nothing framework-specific. These are the most common issues real users encounter:

**Spring Boot:**
- `@Cacheable` without eviction → unbounded `ConcurrentHashMap` growth. MAT pattern: `CaffeineCache` → `LocalManualCache` → `ConcurrentHashMap`
- Hibernate L1 session not cleared in batch jobs → `PersistenceContext` retaining millions of entities. Fix: `session.flush(); session.clear()` every N rows
- Hibernate L2 cache on high-churn entities → stale entity graphs in `EhcacheRegionFactory`
- Spring Boot `/actuator/heapdump` endpoint exposure — active exploitation confirmed (Staples $5M fraud 2023, Amadeus 2026, TeleMessage 2025)

**Quarkus:**
- CDI `@Dependent` bean leak with Fault Tolerance `@Fallback` (issue #38824) — `EagerInstanceHandle` retaining job objects
- Reactive REST client leak — one retained `CompletionStage` per outgoing async call (issue #54639)
- Native image vs JVM mode — different memory semantics; RSS ≠ JVM heap

**Jakarta EE:**
- CDI Event Observer leaks — `@Observes` on beans with incorrect scope
- EJB timer service leaks — stateful session beans never removed
- Shared `ManagedExecutorService` retaining closures capturing request state

**Sources:** [Hibernate Under the Hood series (2024)](https://dev.to/silver_dev/series/45440), [Quarkus #38824](https://github.com/quarkusio/quarkus/issues/38824), [safeguard.sh Amadeus report](https://safeguard.sh/resources/blog/amadeus-booking-data-exposures-korean-air-asiana-lufthansa-condor-october-2026), [SANS ISC Diary](https://isc.sans.edu/diary/33188), [Wiz Blog Dec 2024](https://www.wiz.io/blog/spring-boot-actuator-misconfigurations)

---

### 2. Kubernetes Container Memory Analysis (Not Covered)

The **#1 mistake** in Kubernetes heap analysis: assuming `OOMKilled` = Java heap leak.

**Key insight**: `-XX:+HeapDumpOnOutOfMemoryError` does NOT fire on kernel OOMKill — only on JVM `OutOfMemoryError`. The kernel sends `SIGKILL` when `memory.current` exceeds `memory.max`.

**Container memory hierarchy:**
```
Container memory limit (cgroup memory.max)
  └── JVM heap (-Xmx)
  └── Metaspace
  └── Thread stacks (N threads × ~512KB)
  └── Code cache
  └── Direct ByteBuffers
  └── JNI / native allocations
  └── glibc malloc fragmentation
```

**Diagnostic sequence for OOMKilled pods:**
1. `kubectl describe pod` — confirm OOMKill (Exit Code: 137)
2. `kubectl logs --previous` — capture logs before they're lost (~90s TTL)
3. Check cgroup files: `memory.stat`, `memory.events`, `memory.current`
4. `jcmd VM.native_memory summary` — was it heap or off-heap?
5. `java -XshowSettings:system -version` — what limit did the JVM detect?

**Sizing rule:** Use `-XX:MaxRAMPercentage=75` (not absolute `-Xmx`) for container-aware sizing.

**Evidence preservation:** K8s 1.28+ native sidecar containers (`restartPolicy: Always` in `initContainers`) can copy heap dumps to persistent storage before the main container dies.

**Interpretation matrix:**

| Pattern | Diagnosis |
|---------|-----------|
| Heap grows, trend-line is flat post-GC | Normal — not a leak |
| Heap grows, trend-line rises post-GC | Slow leak — growing live set |
| Heap grows, barely drops after GC | Fast leak or over-large cache |
| RSS grows but heap histogram stable | Off-heap growth → NMT analysis |
| Metaspace grows, heap stable | Classloader leak |

**Sources:** [Kubernetes native sidecars blog (2023)](https://kubernetes.io/blog/2023/08/25/native-sidecar-containers/), [GKE OOM Troubleshooting](https://docs.cloud.google.com/kubernetes-engine/docs/troubleshooting/oom-events), [Arvo-AI/lastwords](https://github.com/Arvo-AI/lastwords)

---

### 3. Off-Heap / Native Memory Analysis (Not Covered)

When RSS grows but heap dumps look healthy, the problem is off-heap. The skill covers DirectByteBuffer in the context of report sections but not the full NMT workflow.

**NMT analysis sequence:**
```bash
# Must be enabled at startup (cannot attach to running JVM)
-XX:NativeMemoryTracking=summary

# Establish baseline after warmup
jcmd <pid> VM.native_memory baseline

# Wait for memory to grow, then diff
jcmd <pid> VM.native_memory summary.diff

# Example output interpretation:
# Java Heap: 512MB (+0)        ← heap is fine
# Class (Metaspace): 128MB (+50MB) ← classloader leak?
# Thread: 256MB (+80MB)        ← thread leak?
# Code: 512MB (+70MB)          ← JIT pressure
```

**NMT blind spots:** Does NOT see native library `malloc`, glibc arena fragmentation, or GPU memory. When RSS > NMT total, use `pmap <pid>` + native profilers (jemalloc/jeprof).

**DirectByteBuffer monitoring:**
```bash
# JMX: java.nio:type=BufferPool,name=direct → MemoryUsed, Count
# Soft cap: MaxDirectMemorySize defaults to ~Xmx if not specified
jcmd <pid> VM.flags | grep MaxDirectMemorySize
```

**Heavy DirectByteBuffer users:** Netty (Vert.x, Quarkus reactive, Spring WebFlux), gRPC, Kafka Streams, Apache Arrow, Chronicle Map.

**Sources:** [java.harshpreet.com — native memory](https://java.harshpreet.com/learn/gc/native-memory), [unixy.io — JVM memory beyond the heap](https://unixy.io/blog/jvm-memory-beyond-the-heap/), [CloudBees KB](https://docs.cloudbees.com/docs/cloudbees-ci-kb/latest/client-and-managed-controllers/controller-oomkilled-due-to-native-memory-growth-not-visible-in-nmt)

---

### 4. Classloader Leak Diagnostic Workflow (Not Covered)

A classloader leak = a classloader that should have been GC'd after undeploy/redeploy is still reachable. All its classes remain in Metaspace. Symptom: `OutOfMemoryError: Metaspace` after N redeploys.

**Step-by-step workflow:**
1. **Confirm before dumping:** `jcmd <pid> VM.classloader_stats` before and after redeploy — count increases = confirmed leak
2. **Check class count:** `jcmd <pid> GC.class_histogram -all=true | head -50`
3. **Capture dump:** `jcmd <pid> GC.heap_dump`
4. **In hprof-analyzer:** Run "Duplicate Classes" analysis, find objects of your classloader type, trace GC root path

**Common root causes:**

| Root Cause | Detection |
|------------|-----------|
| Static collection in shared library (Log4j, JDBC DriverManager) | Static field → HashMap → your classloader |
| `ThreadLocal` in pooled thread | Thread → `threadLocals` → your classloader |
| JMX MBean not unregistered | `MBeanServer` → descriptor → classloader |
| Shutdown hook never removed | `ApplicationShutdownHooks` → Thread → classloader |
| JDBC driver auto-registration | `DriverManager.registeredDrivers` → your Driver class |
| Spring DevTools hot reload | `RestartClassLoader` leak |

**Sources:** [ggorantala.dev — Metaspace guide (2024)](https://www.ggorantala.dev/outofmemoryerror-metaspace/), [java.harshpreet.com — classloader leaks](https://java.harshpreet.com/learn/deep-dives/classloader-leaks)

---

### 5. Tiered Leak Detection Strategy (Not Covered)

The skill jumps straight to heap dumps. Modern practice uses a tiered approach:

**Tier 1 — JFR OldObjectSample (always-on, <1% overhead)**
```bash
-XX:StartFlightRecording=settings=profile,maxage=24h,maxsize=500m
jfr print --events OldObjectSample recording.jfr
```
Gives: allocation stack trace + path to GC root for objects surviving multiple GCs. Safe for permanent production recording.

**Tier 2 — Class Histogram Diff (medium overhead, point-in-time)**
```bash
jcmd <pid> GC.class_histogram > hist1.txt
sleep 600
jcmd <pid> GC.class_histogram > hist2.txt
diff hist1.txt hist2.txt | grep "^>"
```

**Tier 3 — Full Heap Dump Comparison**
Only when Tiers 1/2 identify a suspect class but the retention chain is unknown.

**Sources:** [Oracle Troubleshoot Memory Leaks (JDK 21–25)](https://docs.oracle.com/en/java/javase/24/troubleshoot/troubleshooting-memory-leaks.html), [happycoders.eu JFR](https://www.happycoders.eu/java/java-flight-recorder/)

---

### 6. Heap Dump Security — Real-World Attacks (Partially Covered)

The skill covers `detect-secrets` and `redact` but not the security threat model.

**Confirmed real-world incidents:**
- **Staples (2023):** Repeated heap dump requests against internal system → captured session tokens → $5M gift card fraud ([Gizmodo](https://gizmodo.com/chinese-crime-ring-allegedly-hacked-staples-to-mint-5-million-in-gift-cards-2000828269))
- **Amadeus (2026):** Spring Boot heapdump endpoint → admin passwords → global booking system access ([safeguard.sh](https://safeguard.sh/resources/blog/amadeus-booking-data-exposures-korean-air-asiana-lufthansa-condor-october-2026))
- **TeleMessage (2025):** Actuator endpoint → session data extraction ([BankInfoSecurity](https://www.bankinfosecurity.com/attackers-target-legacy-code-in-telemessages-signal-clone-a-29008))
- **DDoSecrets (2025):** Published 410 GB of heap dumps containing production credentials

**Data at risk in heap dumps:**

| Data Type | Why It's in the Heap |
|-----------|---------------------|
| Database credentials | JDBC connection strings, DataSource properties |
| JWT/session tokens | Active HTTP session objects |
| API keys / OAuth tokens | Configuration properties, Environment beans |
| Decrypted secrets | Post-vault-decryption materialized values |
| PII / request payloads | Buffered HTTP request/response bodies |
| Cryptographic keys | `SecretKey`/`PrivateKey` from KeyStore loads |

**Security controls:**
1. Separate management port (`management.server.port=9090`) firewalled to monitoring only
2. Spring Boot 3.5+: heapdump defaults to `access=NONE`
3. Dump files: `0600` permissions, encrypted at rest, 7-day retention policy
4. Never upload to vendor portals, Jira, or external tools
5. Use `hprof-analyzer redact` before any external transfer

**Sources:** [SANS ISC Diary](https://isc.sans.edu/diary/33188), [Wiz Blog (Dec 2024)](https://www.wiz.io/blog/spring-boot-actuator-misconfigurations), [Spring Boot 3.5 Release Notes](https://github.com/spring-projects/spring-boot/wiki/Spring-Boot-3.5-Release-Notes)

---

## Priority 2 — Valuable Context & Complementary Techniques

### 7. Kotlin Coroutine Heap Patterns (Not Covered)

Suspended coroutines store their full local variable state in continuation objects. Key classes in heap dumps:
- `CancellableContinuationImpl`, `ScopeCoroutine`, `SuspendLambda`, `ChildContinuation`

**Common leak patterns:**

| Pattern | Root Cause | Fix |
|---------|-----------|-----|
| `GlobalScope` + long-lived coroutines | Never cancelled | Use `lifecycleScope`/`viewModelScope` |
| `UndispatchedCoroutine` + custom interceptor | `ThreadLocalMap` stale entries | Use standard dispatchers |
| `ChildContinuation` accumulation | Race in reusable continuations | Upgrade kotlinx.coroutines |

**Diagnostic tool:** `DebugProbes.dumpCoroutines()` from `kotlinx-coroutines-debug` — maps continuation objects to source locations.

**Lambda closures:** `$` in class names (e.g., `MyClass$$Lambda$42`) with `this$0` fields capturing the enclosing object. `by lazy {}` captures `this` of the enclosing class until first access.

**Sources:** [Kotlin coroutines codegen](https://github.com/JetBrains/kotlin/blob/master/compiler/backend/src/org/jetbrains/kotlin/codegen/coroutines/coroutines-codegen.md), [kotlinx.coroutines #4615](https://github.com/Kotlin/kotlinx.coroutines/issues/4615)

---

### 8. JFR + Heap Dump Correlation (Not Covered)

JFR and heap dumps answer complementary questions:

| Question | JFR | Heap Dump |
|----------|-----|-----------|
| Where is memory being allocated? | `ObjectAllocationSample` (call-site flamegraphs) | No allocation provenance |
| What is surviving GC? | `OldObjectSample` (partial) | Full object graph |
| Why is it retained? | No reference chains | Dominator tree, reference paths |
| Heap growth trend over time | Continuous | Point-in-time snapshot |
| Production-safe? | <2% overhead | Stop-the-world |

**Recommended combined workflow:**
1. Start JFR: `jcmd <pid> JFR.start duration=300s filename=app.jfr settings=profile`
2. Analyze allocation hotspots and OldObjectSample trends in JMC
3. If leak confirmed, take heap dump for retention path analysis
4. Correlate: JFR tells you *who allocates*, heap dump tells you *who retains*

**JMC 9.1 (2024):** Added RSS display — diagnoses native memory leaks where RSS grows while Java heap is stable.

**Sources:** [Oracle JMC 9 docs](https://docs.oracle.com/en/java/java-components/jdk-mission-control/9/user-guide/whats-new-jdk-mission-control.html), [Inside Java JMC intro 2024](https://inside.java/2024/05/18/jmc-intro/)

---

### 9. Production Incident Response Workflow (Not Covered)

The skill explains *how to use hprof-analyzer* but not *how to handle a production memory incident end-to-end*.

**SRE best-practice sequence:**
1. **Declare & scope** — Assign severity, open incident channel
2. **Stop the bleeding** — Restore service first, preserve evidence second
3. **Verify dump is necessary** — Could it be allocation churn, GC tuning, or off-heap?
4. **Pre-flight checklist:**
   - Check disk space (`df -h`; dump ≈ `-Xmx` size)
   - Confirm approved storage path (not web-accessible)
   - Get explicit approval (PII/secrets risk)
   - Document chain-of-custody
5. **Capture safely** — `jcmd <pid> GC.heap_dump` (not deprecated `jmap`)
6. **Analyze locally** — Never upload to external tools
7. **Correlate** — Compare with GC logs, latency graphs, recent deploys
8. **Runbook output** — Convert findings to tracked remediation

**Common beginner mistakes:**
- Taking dump before checking disk space (can fill filesystem, worsening incident)
- Uploading to vendor/Jira (exposes secrets)
- Using `jmap` directly (deprecated; `jcmd` is canonical)
- Diagnosing heap when the problem is off-heap
- Analyzing only shallow size instead of retained size

**Sources:** [Acceldata JVM Troubleshooting Guide](https://documentation.acceldata.io/docs/acceldata/odp/odp-3-4-3-0/documentation/troubleshooting-jvm-jstack-heap-dumps-and-jfr-22-2), [Awesome GitHub Copilot Java Diagnostics](https://awesome-copilot.github.com/skill/java-diagnostics/)

---

### 10. CI/CD Integration Patterns (Not Covered)

**The modern pattern:** Threshold-gated hybrid analysis — not full heap dump on every commit.

```
Every build/test:
  → Lightweight JFR recording (default profile)
  → If post-GC live set grows beyond threshold → trigger heap dump
  → Run automated leak-suspect analysis
  → Fail build if suspects above severity threshold
```

**Notable patterns:**
- **Camunda (2024):** AI-agent parallel heap dump analysis at CI scale ([PR #67396](https://github.com/camunda/camunda/pull/67396))
- **DataDog:** `DumpHangedTestPlugin` Gradle plugin auto-captures heap+thread dumps before test timeout
- **hprof-analyzer for CI:** JSON output is stable enough to diff; gate builds on retained-size regressions

**Sources:** [Camunda PR #67396](https://github.com/camunda/camunda/pull/67396), [DataDog DeepWiki](https://deepwiki.com/DataDog/dd-trace-java/1.2-getting-started-and-contributing)

---

## Priority 3 — Cross-Ecosystem Inspiration (Future Feature Ideas)

Ideas from other language ecosystems that could inspire new hprof-analyzer views:

### From .NET (dotMemory)

- **Automatic Smart Inspections:** One-click heuristic checks — duplicate strings, sparse arrays, finalizer queue buildup, event handler leaks. hprof-analyzer's triage section partially covers this but could be expanded with more heuristics.
- **GC Generation Cross-Reference:** Which old-gen objects reference young-gen objects preventing collection? Not available in JVM heap dumps directly but could be approximated with JFR correlation.
- **Async State Machine Detection:** Group heap objects by lambda/continuation ancestry (CompletableFuture chains, virtual thread continuations, Kotlin coroutines).

**Sources:** [JetBrains dotMemory Inspections](https://www.jetbrains.com/help/dotmemory/Inspections.html), [dotMemory Generations](https://www.jetbrains.com/help/dotmemory/Generations.html)

### From Go (pprof)

- **Dual-Mode Profile (inuse vs alloc):** Retained memory vs allocation churn from the same data — JFR + heap dump correlation could achieve this.
- **Thread/Goroutine Leak Detection:** Group threads by blocking call site, flag monotonically growing concentrations. Especially valuable for virtual thread (Project Loom) applications.
- **Differential Flame Graph:** Red = grew, blue = shrank between two snapshots. More actionable than class-level diffs.

**Sources:** [Go pprof tutorial](https://labs.iximiuz.com/tutorials/go-pprof-tutorial-79c6591d), [Go goroutine leak profiles](https://go.dev/blog/goroutine-leak-profiles), [Uber LeakProf](https://www.uber.com/tw/en/blog/leakprof-featherlight-in-production-goroutine-leak-detection/)

### From Python (Memray)

- **Temporal Allocation Flame Graph:** Select a time window, see what was allocated then that's still live. Uses JFR allocation events + GC events for the JVM equivalent.

**Sources:** [Bloomberg Memray](https://bloomberg.github.io/memray/), [Memray temporal flame graphs](https://github.com/bloomberg/memray/discussions/395)

### From Rust (DHAT)

- **Three-Moment Model:** Per allocation site, show (1) total allocated (churn), (2) live at heap peak (who caused the crisis), (3) live in current dump (who leaked). Separates allocation-heavy-but-well-GC'd code from leaking code.
- **Block Lifetime Metrics:** Average object lifetime by allocation site. High allocation rate + low lifetime = GC churn. High retained + low reads = zombies.

**Sources:** [DHAT docs](https://edf-hpc.github.io/verrou/dh-manual.html), [A better DHAT — Mozilla](https://blog.mozilla.org/nnethercote/2019/04/17/a-better-dhat/)

### From Chrome DevTools (V8)

- **Five-View Instant Switching:** Summary / Comparison / Containment / Statistics / Dominators — all from the same snapshot, preserving object selection across views.
- **Retainers Panel:** After selecting any object, automatically show top-3 shortest paths to GC roots. Multiple parallel paths shown simultaneously.

**Sources:** [Chrome DevTools heap snapshots](https://developer.chrome.com/docs/devtools/memory-problems/heap-snapshots)

### From iOS (Xcode Instruments)

- **Mark Epoch / Generation Checkpoint:** User-defined named checkpoints. Show objects allocated after epoch N that are still live — answers "did operation X leak?"
- **Neighborhood View:** Both incoming and outgoing references for a selected object simultaneously, with depth sliders.

**Sources:** [WWDC24 — Analyze heap memory](https://developer.apple.com/videos/play/wwdc2024/10173/), [Xcode Memory Graph Debugger docs](https://developer.apple.com/documentation/xcode/gathering-information-about-memory-use)

---

## Priority 4 — Academic/Advanced (Reference Material)

### Deep Subgraph Deduplication (jafar)

Beyond duplicate strings: `jafar` detects **structurally identical object subgraphs** using FNV-1a fingerprints over class, fields, and referenced subtrees down to configurable depth. Groups sorted by `wastedBytes`. hprof-analyzer already has duplicate string/array detection; subgraph dedup would be a significant extension.

**Source:** [jafar HdumpPath](https://github.com/btraceio/jafar/blob/main/doc/hdumppath.md)

### Annotation-Driven Analysis

No mainstream tool groups objects by Java annotation — a genuine gap. Potential views:
- Group by `@Entity`, `@Service`, `@Controller`, `@Repository` → retained memory per annotation type
- Spring bean graph from `DefaultListableBeanFactory.beanDefinitionMap` with retained size overlay
- Framework layer memory pie: Spring infrastructure vs JPA/Hibernate vs application objects vs third-party

**Source:** [PLDI 2020 enterprise static analysis](https://yanniss.github.io/enterprise-pldi20.pdf)

### ML/Anomaly Detection on Heap Data

- **AntTracks (JKU):** Suspicious time window detection — continuous growth, GC utilization spikes, memory churn. Rule-based anomaly detection. ([FSE 2019](https://dl.acm.org/doi/10.1145/3357390.3361025))
- **ScaMiner (ICDM 2023):** Subgroup discovery on class histograms to find classes consuming more memory than expected vs user background knowledge. ([arXiv](https://arxiv.org/html/2310.00781v1))
- **Resin (OSDI 2022):** Holistic leak detection service using bucketization-based pivot detection to trigger targeted snapshots. ([USENIX](https://www.usenix.org/system/files/osdi22-lou-resin.pdf))
- **Oracle Patent WO2014168981:** Seasonal trending and OOM crash date forecasting from post-GC heap floors.

### Historical Trend Visualization

- **AntTracks TrendViz (ICPE 2019):** Track heap object groups over time, display growth per group. ([ACM](https://dl.acm.org/doi/10.1145/3302541.3313100))
- **AntTracks Time-Travel (EuroVis 2020):** Space-filling tree of heap at one time, scrub through history. ([EuroVis](https://diglib.eg.org/items/a44a0f98-e747-4540-8259-8a4bbd38c5e6))

### GraalVM Native Image Heap Analysis

- Standard HPROF format, openable in VisualVM (MAT may not resolve synthetic classes)
- Requires `--enable-monitoring=heapdump,jvmstat` at build time
- Trigger: SIGUSR1, `jcmd GC.heap_dump`, or `-XX:+HeapDumpOnOutOfMemoryError`
- NMT only summary mode in native images; no async-profiler support

**Source:** [GraalVM Native Image Heap Dump Guide](https://www.graalvm.org/latest/reference-manual/native-image/guides/create-heap-dump/)

### OpenJ9 Three-Dump Model

- Heap dump + Javacore (threads/locks/env) + System dump (native state) — analyzed together via `jdmpview` and DTFJ-enabled MAT
- **Security advisory CVE-2026-97400:** Opening untrusted OpenJ9 diagnostic bundles with `jdmpview` can cause arbitrary file read + remote exfiltration. Analyze on sandboxed machines.

**Source:** [Eclipse OpenJ9 jdmpview docs](https://eclipse.dev/openj9/docs/tool_jdmpview/), [GHSA-548h-r226-j3rq](https://github.com/advisories/GHSA-548h-r226-j3rq)

### async-profiler v4.0+ Native Memory Leak Profiling

Intercepts `malloc`/`realloc`/`calloc`/`free` at the native level. Unfreed allocations at recording end → flame graph of leak sites. Bridges the JVM/native boundary that heap dumps cannot see.

**Source:** [async-profiler v4.0 release](https://github.com/async-profiler/async-profiler/releases/tag/v4.0), [v4.1 release](https://github.com/async-profiler/async-profiler/releases/tag/v4.1)

---

## Summary: What to Add to the Skill

| # | Topic | Priority | Why |
|---|-------|----------|-----|
| 1 | Framework-specific patterns (Spring, Quarkus, Jakarta EE) | P1 | Most common real-world issues users face |
| 2 | Kubernetes container memory (OOMKill vs JVM OOM) | P1 | Critical misunderstanding; saves hours of misdirected analysis |
| 3 | Off-heap / NMT analysis workflow | P1 | Covers the "heap looks fine but RSS grows" blind spot |
| 4 | Classloader leak diagnostic workflow | P1 | Specific, actionable, commonly needed |
| 5 | Tiered leak detection (JFR → histogram → dump) | P1 | Modernizes the workflow; avoids unnecessary dumps |
| 6 | Heap dump security threat model | P1 | Real attacks; users need to know the risks |
| 7 | Kotlin coroutine heap patterns | P2 | Growing user base; unique heap signatures |
| 8 | JFR + heap dump correlation | P2 | Complementary techniques; allocation provenance |
| 9 | Production incident response workflow | P2 | End-to-end guidance; common mistakes to avoid |
| 10 | CI/CD integration patterns | P2 | Automated analysis in pipelines |
| 11 | Cross-ecosystem analysis ideas | P3 | Future feature inspiration for hprof-analyzer |
| 12 | Academic/ML anomaly detection | P4 | Long-term research direction |

# Results

## Run 1 — 23_NormalDomainObjectGraph (HEALTHY)
Verdict: NO leak (CORRECT). BUT reached it by READING THE JAVA SOURCE comment
("THIS IS NOT A BUG"), not the MCP. The MCP triage gave 3 CRITICAL signals
(headline-retainer 97.4%, concentration, big-drop-concentration) on a healthy
heap → would have misled a source-blind agent to "yes, leaking". Tools cried wolf.

## Run 2 — 01_StaticCacheLeak (LEAK; truth: static CACHE HashMap)
Verdict: YES leak (correct) BUT culprit WRONG: hallucinated "javac internal
compiler cache / MemoryContext/MemoryClassLoader" held by Thread. The static
CACHE owner was invisible → agent invented a JDK-internal cause from the
Thread+HashMap shape. ❌ wrong root cause. Anchored on triage "Thread retains
95.6%". (GAP-1 fix would name _01_StaticCacheLeak.)

## Run 4 — 18_OversizedHashMapWaste (WASTE, not a leak)
Verdict: "YES - critical memory leak" ❌ (it's waste, not a leak). Attributed to
"Oversized HashMap held by Thread". Called the dump filename as a tell. Triage
headline = "Thread retains 97.8%". Waste-vs-leak conflated; Thread misattribution.

## Run 5 — 29_HashMapHighCollisionRate (perf/waste, not a leak)
Verdict: "YES severe leak / OOM" ❌. SERIOUS: retention chain names
"_23_NormalDomainObjectGraph" and "80,000 Product nodes" — i.e. it reported
dump 23's structure while tasked with 29. Possible MCP session/cache BLEED
across concurrent load_dump calls (multiple agents sharing one MCP server?),
OR the agent confused sessions. FLAG: investigate whether concurrent MCP
load_dump calls collide. Also: perf issue (collisions) framed as a leak/OOM.

## Run 3 — 05_ThreadLocalLeak: (pending / not yet returned)

## Run 6 — 23_NormalDomainObjectGraph (HEALTHY; truth: NO leak)
Verdict: "YES, LEAK" ❌ FALSE POSITIVE. Source-blind, believed triage "critical
headline-retainer/concentration 97.4%", hallucinated "javac MemoryContext
compiler context". The healthy cache is reported as a critical leak. KEY RESULT.

## Run 3 — 05_ThreadLocalLeak (LEAK; truth: static ThreadLocal byte[] accumulation)
Verdict: YES threadlocal leak (direction ~right) BUT names "_01_StaticCacheLeak"
+ HashMap(6449) — wrong dump's identifiers.

## ⚠ SUSPECTED MCP CONCURRENCY/SESSION-BLEED BUG
Across runs 2(01),3(05),5(29),6(23) the SAME fingerprint recurs: object_index
6449, 4241 HashMap$Node, 62.8MB, and names from the WRONG dump (_23_, _01_,
MemoryContext). 5 agents ran concurrently sharing ONE MCP server (session's
registered `hprof`). Concurrent load_dump calls appear to clobber a shared
session → agents read whichever dump was loaded last. MUST VERIFY (below).

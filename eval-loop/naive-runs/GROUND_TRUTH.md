# Naive-agent experiment: 20 runs, "find the issues in this heap dump"

Each Haiku agent gets ONLY a dump path + "find the issues", no hints, MCP tools.
I record its verdict + tool sequence and score vs ground truth below.

Dump rotation (warm caches) and ground truth:
- 01_StaticCacheLeak      → LEAK: static CACHE HashMap (unbounded). Culprit class: _01_StaticCacheLeak.
- 04_UnboundedCollectionLeak → LEAK: static EVENT_LOG ArrayList→Object[]→LogEntry.
- 05_ThreadLocalLeak      → LEAK: static ThreadLocal per-thread byte[] accumulation.
- 18_OversizedHashMapWaste → WASTE (not a leak): over-capacity/empty collections.
- 21_DuplicateStringWaste → WASTE (not a leak): duplicate String values.
- 23_NormalDomainObjectGraph → HEALTHY (NO leak): intentional 80k-Product CATALOG cache.
- 29_HashMapHighCollisionRate → WASTE/perf (not a leak): pathological hash collisions.

Scoring per run: correct verdict? named right culprit? did it rely on the MCP
or external (source) context? which tool output misled it?

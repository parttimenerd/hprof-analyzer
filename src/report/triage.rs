//! OOM-triage rule framework: the single source of truth for the "OOM Triage"
//! section. Each rule reads the finished [`Report`] and either fires (emitting a
//! [`TriageSignal`]) or stays silent. [`evaluate_triage`] runs every rule once,
//! in registry order, and the result is stored on `Report.triage`. Both the
//! Markdown and HTML renderers are dumb formatters over that list, so rule logic
//! lives in exactly one place.
//!
//! A rule "declares the data it needs" implicitly by which `Report` fields it
//! reads in `eval`; each rule's doc-comment states that dependency explicitly.

use crate::report::format::{fmt_count, format_bytes};
use crate::report::model::{Report, TriageSeverity, TriageSignal};

// ── Thresholds ────────────────────────────────────────────────────────────────
// Each rule fires only when its signal crosses one of these. Kept together so
// the whole triage policy is visible in one place.

/// If the single largest suspect retains at least this share of the reachable
/// heap, the heap is called "highly concentrated".
const CONCENTRATION_PCT: f64 = 50.0;
/// DirectByteBuffer capacity floor (bytes) before the off-heap rule fires.
const DBB_FLOOR_BYTES: u64 = 64 * 1024 * 1024;
/// Unreachable-shallow share of total heap at which the GC-waste rule fires.
const GC_WASTE_RATIO: f64 = 0.10;
/// A single thread retaining at least this share of the heap flags pinning.
const THREAD_PIN_PCT: f64 = 20.0;
/// …or a thread holding at least this many live thread-local roots, provided it
/// also retains at least [`THREAD_PIN_LOCALS_MIN_PCT`] of the heap (the min-share
/// gate keeps normal threads like `main`, which hold many roots at trivial
/// retention, from tripping the rule).
const THREAD_PIN_LOCALS: u64 = 100;
/// Minimum retained share for the many-local-roots branch to fire.
const THREAD_PIN_LOCALS_MIN_PCT: f64 = 10.0;
/// Top GC-root type retaining at least this share of the heap.
const GC_ROOT_DOMINANT_PCT: f64 = 50.0;
/// Anonymous/generated classes as a share of all loaded classes.
const PROXY_BLOAT_PCT: f64 = 50.0;
/// Ignore proxy/lambda bloat on dumps with fewer than this many classes.
const PROXY_MIN_CLASSES: u64 = 200;
/// Objects reachable only via soft/weak/phantom refs before the escape rule.
const WEAKREF_FLOOR: u64 = 1000;
/// Retained bytes reachable only via soft/weak/phantom refs before the escape rule.
const WEAKREF_BYTES_FLOOR: u64 = 5 * 1024 * 1024; // 5 MB
/// Wasted collection backing-array bytes as a share of heap.
const OVERCAP_WASTE_PCT: f64 = 5.0;
/// Total shallow bytes in constant-value primitive arrays before the rule.
const CONSTARR_FLOOR: u64 = 8 * 1024 * 1024;
/// Fill ratio (basis points) below which a collection counts as "under-filled".
const OVERCAP_FILL_BP: u32 = 5000;
/// Duplicate-String waste floor (bytes) before the duplicate-strings rule fires.
const DUP_STRINGS_FLOOR_BYTES: u64 = 16 * 1024 * 1024;
/// …or duplicate-String waste as a share of the heap.
const DUP_STRINGS_PCT: f64 = 5.0;
/// char[]/byte[] backing-array slack floor (bytes) for the char-array-slack rule.
const CHAR_SLACK_FLOOR_BYTES: u64 = 16 * 1024 * 1024;
/// …and a minimum count of wasteful arrays, so a handful of big ones don't fire.
const CHAR_SLACK_MIN_ARRAYS: u64 = 1000;
/// Boxed-primitive instance-count floor before the boxed-bloat rule fires.
const BOXED_FLOOR_INSTANCES: u64 = 5_000_000;
/// …or boxed-primitive shallow as a share of the heap.
const BOXED_PCT: f64 = 5.0;
/// A single collection with at least this many elements is called "unbounded".
const UNBOUNDED_COLL_ELEMENTS: u64 = 1_000_000;
/// …or one collection retaining at least this share of the heap.
const UNBOUNDED_COLL_PCT: f64 = 20.0;
/// Live-instance floor for the object-swarm rule (one tiny class, huge count).
const SWARM_FLOOR_INSTANCES: u64 = 10_000_000;
/// …its aggregate shallow as a share of the heap.
const SWARM_PCT: f64 = 10.0;
/// …and a per-instance shallow ceiling (bytes): swarms are many *small* objects.
const SWARM_MAX_INSTANCE_BYTES: u64 = 64;
/// Live ClassLoader-instance count before the classloader-explosion rule fires.
const CLASSLOADER_EXPLOSION_FLOOR: u64 = 1000;
/// Live-thread count before the thread-swarm rule fires.
const THREAD_SWARM_FLOOR: usize = 1000;
/// `java.lang.ref.Finalizer` instance count that signals a backed-up queue.
const FINALIZER_FLOOR: u64 = 10_000;
/// Loaded-class count above which Metaspace pressure is likely.
const METASPACE_CLASS_FLOOR: u64 = 50_000;
/// Combined reflect.{Method,Field,Constructor} instances suggesting unbounded caches.
const REFLECT_FLOOR: u64 = 500_000;
/// "JNI Global" root count that, together with a retained-share threshold,
/// indicates a JNI global-reference leak.
const JNI_GLOBAL_FLOOR: u64 = 5_000;
/// Minimum retained share for the JNI-global rule to fire.
const JNI_GLOBAL_RETAINED_PCT: f64 = 5.0;
/// Single heap-composition kind share that constitutes "skew".
const HEAP_SKEW_PCT: f64 = 70.0;
/// Suspect retained share at which the static-field-anchor rule fires.
const STATIC_ANCHOR_PCT: f64 = 20.0;
/// Session/request-scope class instance floor (name-pattern gate).
const SESSION_FLOOR: u64 = 100_000;
/// Connection/socket class instance floor (name-pattern gate).
const CONNECTION_FLOOR: u64 = 1_000;
/// Listener/observer class instance floor (name-pattern gate).
const LISTENER_FLOOR: u64 = 100_000;
/// Parser-output class instance floor (package-pattern gate).
const PARSER_FLOOR: u64 = 100_000;
/// String instance count + JNI global count that together signal intern() abuse.
const INTERNED_STRING_FLOOR: u64 = 2_000_000;
const INTERNED_JNI_FLOOR: u64 = 1_000;
/// Object-array fill ratio (bp) below which arrays are "sparse"; must have
/// >= this many tracked arrays and wasted share >= SPARSE_ARRAY_WASTED_PCT.
const SPARSE_ARRAY_FILL_BP: u32 = 2_000; // 20%
const SPARSE_ARRAY_MIN_TRACKED: u64 = 10_000;
const SPARSE_ARRAY_WASTED_PCT: f64 = 5.0;
/// Big-drop node drop_bytes as share of total shallow heap.
const BIG_DROP_PCT: f64 = 5.0;
/// Big-drop absolute floor (bytes).
const BIG_DROP_FLOOR: u64 = 64 * 1024 * 1024;
/// Object header overhead share above which the fixed-per-object rule fires.
const HEADER_OVERHEAD_PCT: f64 = 20.0;
/// Hash-map collision ratio (load-factor proxy in bp) above which hotspot fires.
/// Bucket upper_ratio_bp > COLLISION_HIGH_BP means the map is very dense.
const COLLISION_HIGH_BP: u32 = 9_000; // > 90% load → chain collisions likely
/// Minimum collision-ratio tracked maps for the rule to fire.
const COLLISION_MIN_TRACKED: u64 = 100;
/// Empty-collection share above which the cemetery rule fires.
const EMPTY_COLL_SHARE_PCT: f64 = 60.0;
/// Absolute empty-collection count floor.
const EMPTY_COLL_FLOOR: u64 = 500_000;
/// Single primitive array shallow bytes as share of heap.
const OVERSIZED_PRIM_ARRAY_PCT: f64 = 5.0;
/// Absolute floor for the oversized-primitive-array rule.
const OVERSIZED_PRIM_ARRAY_FLOOR: u64 = 64 * 1024 * 1024;
/// Duplicate-primitive-array wasted bytes as share of heap.
const DUP_PRIM_ARRAYS_PCT: f64 = 5.0;
/// Duplicate-primitive-array absolute wasted-bytes floor.
const DUP_PRIM_ARRAYS_FLOOR: u64 = 16 * 1024 * 1024;
/// Dominator-chain depth threshold: a longest-chain above this signals a
/// linked-list-shaped heap (unbounded accumulation via linked structure).
const DEEP_CHAIN_DEPTH: u32 = 10_000;
/// Minimum framework retained bytes before the framework-leak rule fires.
const FRAMEWORK_RETAINED_FLOOR: u64 = 64 * 1024 * 1024;
/// Framework retained share of total heap that constitutes a "significant" signal.
const FRAMEWORK_RETAINED_PCT: f64 = 10.0;
/// Minimum retained bytes held by blocked/waiting threads before rule fires.
const BLOCKED_THREAD_RETAINED_FLOOR: u64 = 64 * 1024 * 1024;
/// Blocked/waiting retained as a share of heap.
const BLOCKED_THREAD_PCT: f64 = 10.0;
/// Tiny-collection overhead floor (bytes) before the rule fires.
const TINY_COLL_OVERHEAD_FLOOR: u64 = 8 * 1024 * 1024;
/// Soft-reference referent retained floor (bytes) before the soft-cache rule fires.
const SOFT_CACHE_RETAINED_FLOOR: u64 = 128 * 1024 * 1024;
/// Minimum non-null soft-reference count for the soft-cache rule to fire.
const SOFT_CACHE_REF_FLOOR: u64 = 10_000;
/// Minimum element count in an ownerless collection for the unowned-sink rule.
const UNOWNED_SINK_ELEMENTS: u64 = 100_000;
/// Minimum retained bytes for an ownerless collection.
const UNOWNED_SINK_RETAINED: u64 = 32 * 1024 * 1024;
/// JDK release older than this many days at dump-capture time triggers the stale-JDK rule.
const STALE_JDK_DAYS: i64 = 270; // ~9 months
/// Minimum number of same-class worker objects for the worker-pool retention rule.
const WORKER_POOL_MIN_INSTANCES: u64 = 3;
/// Aggregate retained share of the heap that N same-class workers must hold to fire.
const WORKER_POOL_RETAINED_PCT: f64 = 20.0;
/// Minimum aggregate retained bytes for the worker-pool retention rule.
const WORKER_POOL_RETAINED_FLOOR: u64 = 64 * 1024 * 1024;
/// Minimum aggregate shallow bytes of CGLIB-proxied domain objects before rule fires.
const CGLIB_PROXY_SHALLOW_FLOOR: u64 = 32 * 1024 * 1024;
/// Minimum total instances of CGLIB-proxied domain objects.
const CGLIB_PROXY_INSTANCE_FLOOR: u64 = 50_000;
/// WeakHashMap instance count above which the accumulation rule fires.
const WEAK_HASHMAP_FLOOR: u64 = 100_000;
/// Minimum async-log ring-buffer event count before the rule fires.
/// Ring buffers are sized as powers of 2; >= 512 live instances of RingBufferLogEvent
/// means the buffer is non-trivially populated (default Log4j2 size is 262,144).
const ASYNC_LOG_RINGBUF_FLOOR: u64 = 512;
/// Map-entry instance count threshold: when HashMap$Node / CHM$Node / LHM$Entry
/// together exceed this, accumulated map entries dominate the heap.
const MAP_ENTRY_INSTANCE_FLOOR: u64 = 50_000_000;
/// … or map entries as a share of total live objects.
const MAP_ENTRY_OBJECT_PCT: f64 = 20.0;
/// Hibernate field/setter interceptor instance count floor.
/// A few hundred is normal (one per enhanced field); millions means enhanced entities
/// are being accumulated rather than released after use.
const HIBERNATE_INTERCEPTOR_FLOOR: u64 = 1_000_000;
/// Minimum aggregate shallow bytes for the Hibernate interceptor rule.
const HIBERNATE_INTERCEPTOR_SHALLOW_FLOOR: u64 = 32 * 1024 * 1024;
/// Lock-sync object (ReentrantLock$NonfairSync etc.) instance count floor.
const LOCK_OBJECT_FLOOR: u64 = 500_000;
/// Perf-monitoring call-graph object (CallNode, CallStack, etc.) instance count floor.
const PERF_MONITOR_FLOOR: u64 = 200_000;
/// Minimum total ThreadLocal value retained bytes for the ThreadLocal value retention rule.
const THREADLOCAL_VALUE_RETAINED_FLOOR: u64 = 32 * 1024 * 1024;
/// Minimum entry count for a single ThreadLocal value class to be named in the signal.
const THREADLOCAL_VALUE_ENTRY_FLOOR: u32 = 500;
/// Minimum single primitive-array shallow bytes for the humongous-object rule.
/// G1GC marks objects as "humongous" when they exceed half a region (typically 1–4 MB);
/// 4 MB is a safe conservative threshold that fires on known-problematic allocations.
const HUMONGOUS_ARRAY_FLOOR: u64 = 4 * 1024 * 1024;
/// Top component retained share floor for the component-imbalance rule to fire.
const COMPONENT_IMBALANCE_TOP_PCT: f64 = 60.0;
/// Minimum number of components before the imbalance rule makes sense.
const COMPONENT_IMBALANCE_MIN_COMPONENTS: usize = 3;

const EXCEPTION_ACCUM_FLOOR: u64 = 50_000;
const EXCEPTION_ACCUM_SHALLOW_FLOOR: u64 = 16 * 1024 * 1024;

const DAEMON_RETAINED_PCT: f64 = 15.0;
const DAEMON_RETAINED_FLOOR: u64 = 64 * 1024 * 1024;

// ── Framework ─────────────────────────────────────────────────────────────────

/// A single OOM-triage rule. Reads the finished report; returns `Some` when the
/// signal fires, `None` when it does not.
pub trait Rule {
    fn eval(&self, r: &Report) -> Option<TriageSignal>;
}

/// Ordered rule registry. **Order here is the render order** (show-all-that-fire).
fn rules() -> Vec<Box<dyn Rule>> {
    vec![
        Box::new(HeadlineRetainer),
        Box::new(Concentration),
        Box::new(DominantGcRootType),
        Box::new(Shape),
        Box::new(OneLeakOrMany),
        Box::new(ObjectSwarm),
        Box::new(BoxedPrimitiveBloat),
        Box::new(ClassloaderLeak),
        Box::new(ClassloaderExplosion),
        Box::new(MetaspacePressure),
        Box::new(ThreadLocalLeak),
        Box::new(ThreadPinning),
        Box::new(ThreadSwarm),
        Box::new(WeakRefEscape),
        Box::new(ProxyLambdaBloat),
        Box::new(OffHeap),
        Box::new(GcWaste),
        Box::new(StaticFieldAnchor),
        Box::new(JniGlobalRefLeak),
        Box::new(HeapCompositionSkew),
        Box::new(FinalizerQueueBacklog),
        Box::new(CachedReflectionMetadata),
        Box::new(SessionScopeLeak),
        Box::new(ConnectionLeak),
        Box::new(EventListenerAccumulation),
        Box::new(ParserOutputAccumulation),
        Box::new(InternedStringBloat),
        Box::new(DuplicateStrings),
        Box::new(CharArraySlack),
        Box::new(OverCapacityCollections),
        Box::new(LargeUnboundedCollection),
        Box::new(SparseObjectArrays),
        Box::new(ConstantValueArrays),
        Box::new(BigDropConcentration),
        Box::new(FixedPerObjectOverhead),
        Box::new(HashCollisionHotspot),
        Box::new(EmptyCollectionCemetery),
        Box::new(OversizedPrimArray),
        Box::new(DuplicatePrimArrays),
        Box::new(DeepRetentionChain),
        Box::new(FrameworkLeak),
        Box::new(BlockedThreadConcentration),
        Box::new(TinyCollectionOverhead),
        Box::new(SoftRefCacheExpansion),
        Box::new(UnownedCollectionSink),
        Box::new(StaleJdk),
        Box::new(WorkerPoolRetention),
        Box::new(CglibProxyAccumulation),
        Box::new(WeakHashMapAccumulation),
        Box::new(AsyncLogRingBufferFull),
        Box::new(MapEntryDominance),
        Box::new(HibernateInterceptorAccumulation),
        Box::new(LockObjectProliferation),
        Box::new(PerfMonitoringRetention),
        Box::new(ThreadLocalValueRetention),
        Box::new(HumongousObjectAllocation),
        Box::new(ComponentRetentionImbalance),
        Box::new(ExceptionObjectAccumulation),
        Box::new(DaemonThreadRetention),
    ]
}

/// Evaluate every rule once, in registry order, collecting the ones that fire.
pub fn evaluate_triage(r: &Report) -> Vec<TriageSignal> {
    let mut signals: Vec<TriageSignal> = rules().iter().filter_map(|rule| rule.eval(r)).collect();
    if r.collection_attribution.is_none() {
        signals.push(signal(
            "collections-not-analyzed",
            TriageSeverity::Info,
            "Collection Waste Not Analyzed",
            "Collection waste not analyzed — re-run with `--collections` to check for wasted capacity."
                .to_string(),
            None,
        ));
    }
    signals
}

/// Percentage of total reachable shallow heap. Basis matches the report tables.
fn pct_of(retained: u64, total: u64) -> f64 {
    if total > 0 {
        retained as f64 / total as f64 * 100.0
    } else {
        0.0
    }
}

/// Small `TriageSignal` builder for the common linked case.
fn signal(
    id: &str,
    severity: TriageSeverity,
    title: &str,
    detail: String,
    anchor: Option<(&str, &str)>,
) -> TriageSignal {
    let (anchor, anchor_label) = match anchor {
        Some((a, l)) => (Some(a.to_string()), Some(l.to_string())),
        None => (None, None),
    };
    TriageSignal {
        id: id.to_string(),
        severity,
        title: title.to_string(),
        detail,
        anchor,
        anchor_label,
        bytes: None,
        nav_class: None,
    }
}

fn signal_cls(
    id: &str,
    severity: TriageSeverity,
    title: &str,
    detail: String,
    anchor: Option<(&str, &str)>,
    nav_class: impl Into<String>,
) -> TriageSignal {
    let mut s = signal(id, severity, title, detail, anchor);
    s.nav_class = Some(nav_class.into());
    s
}

// ── Rules (ported from the former render_md.rs hand-written logic) ─────────────

/// Headline retainer. Reads `leaks.suspects` / `top.biggest_objects`. Always
/// fires (the fallback variant names no offender).
struct HeadlineRetainer;
impl Rule for HeadlineRetainer {
    fn eval(&self, r: &Report) -> Option<TriageSignal> {
        let total = r.leaks.total_shallow;
        if let Some(s) = r.leaks.suspects.first() {
            let kind = if s.is_single {
                "a single object"
            } else {
                "a class group"
            };
            Some(signal_cls(
                "headline-retainer",
                TriageSeverity::Critical,
                "Headline Retainer",
                format!(
                    "`{}` ({}) retains {} ({:.1}% of reachable heap).",
                    s.pretty_class,
                    kind,
                    format_bytes(s.retained),
                    pct_of(s.retained, total),
                ),
                Some(("leak-suspects", "Leak Suspects")),
                &s.pretty_class,
            ))
        } else if let Some(o) = r.top.biggest_objects.first() {
            Some(signal_cls(
                "headline-retainer",
                TriageSeverity::Warning,
                "Headline Retainer",
                format!(
                    "`{}` retains {} ({:.1}% of reachable heap).",
                    o.display_class,
                    format_bytes(o.retained),
                    pct_of(o.retained, total),
                ),
                Some(("top-consumers", "Top Consumers")),
                &o.display_class,
            ))
        } else {
            Some(signal(
                "headline-retainer",
                TriageSeverity::Info,
                "Headline Retainer",
                "No dominant retainer found.".to_string(),
                None,
            ))
        }
    }
}

/// Concentration. Reads `leaks.suspects` and (for the owner join) the biggest
/// object's `owner`. Always fires (concentrated vs. diffuse variants).
struct Concentration;
impl Rule for Concentration {
    fn eval(&self, r: &Report) -> Option<TriageSignal> {
        let total = r.leaks.total_shallow;
        let sig = match r.leaks.suspects.first() {
            Some(s) if pct_of(s.retained, total) >= CONCENTRATION_PCT => {
                let kind = if s.is_single {
                    "a single object".to_string()
                } else {
                    format!("a class group of {} instances", s.instance_count)
                };
                let owner = if s.is_single {
                    r.top.biggest_objects.first().and_then(|o| {
                        if o.display_class == s.pretty_class {
                            o.owner.as_deref()
                        } else {
                            None
                        }
                    })
                } else {
                    None
                };
                let held_by = match owner {
                    Some(o) => format!(" held by `{o}`"),
                    None => String::new(),
                };
                signal_cls(
                    "concentration",
                    TriageSeverity::Critical,
                    "Concentration",
                    format!(
                        "highly concentrated — `{}` ({}){} holds {:.1}% of the heap; freeing this object would reclaim most of the heap.",
                        s.pretty_class,
                        kind,
                        held_by,
                        pct_of(s.retained, total),
                    ),
                    Some(("leak-suspects", "Leak Suspects")),
                    &s.pretty_class,
                )
            }
            Some(_) => signal(
                "concentration",
                TriageSeverity::Info,
                "Concentration",
                "diffuse — no suspect exceeds the threshold; retention is spread across multiple roots. Inspect individual suspects to find the most impactful target.".to_string(),
                Some(("leak-suspects", "Leak Suspects")),
            ),
            None => signal(
                "concentration",
                TriageSeverity::Info,
                "Concentration",
                "diffuse — no dominant retainer found; retention is spread evenly across many roots.".to_string(),
                None,
            ),
        };
        Some(sig)
    }
}

/// Dominant GC-root type. Reads `overview.gc_roots_retained_by_type`.
struct DominantGcRootType;
impl Rule for DominantGcRootType {
    fn eval(&self, r: &Report) -> Option<TriageSignal> {
        let total = r.leaks.total_shallow;
        let top = r.overview.gc_roots_retained_by_type.first()?;
        let pct = pct_of(top.retained, total);
        if pct < GC_ROOT_DOMINANT_PCT {
            return None;
        }
        Some(signal(
            "gc-root-type",
            TriageSeverity::Warning,
            "Dominant GC-Root Type",
            format!(
                "{:.1}% of the heap is held by \"{}\" roots — the GC Roots by Type table shows the per-class breakdown.",
                pct, top.root_type,
            ),
            Some(("system-overview", "System Overview")),
        ))
    }
}

/// Shape. Reads `overview.dominator_depth_histogram`.
struct Shape;
impl Rule for Shape {
    fn eval(&self, r: &Report) -> Option<TriageSignal> {
        let hist = &r.overview.dominator_depth_histogram;
        if hist.is_empty() {
            return None;
        }
        let total: u64 = hist.iter().map(|b| b.objects).sum();
        let max_depth = hist.iter().map(|b| b.depth).max().unwrap_or(0);
        let mut cum = 0u64;
        let mut p90 = max_depth;
        for b in hist {
            cum += b.objects;
            if cum * 10 >= total * 9 {
                p90 = b.depth;
                break;
            }
        }
        let shape = if p90 <= 3 {
            "shallow (most objects are held within a few hops of a GC root)"
        } else {
            "deep — long dominator chains suggest nested collections or linked structures; the depth histogram shows the distribution; use the Big Drops table to find the retaining objects"
        };
        Some(signal(
            "shape",
            TriageSeverity::Info,
            "Heap Shape",
            format!("{shape} — 90% of objects within depth {p90}, max depth {max_depth}."),
            Some((
                "dominator-depth-distribution",
                "Dominator-Depth Distribution",
            )),
        ))
    }
}

/// One leak or many. Reads `overview.retention_concentration` and the biggest
/// object's `owner`.
struct OneLeakOrMany;
impl Rule for OneLeakOrMany {
    fn eval(&self, r: &Report) -> Option<TriageSignal> {
        let rc = &r.overview.retention_concentration;
        if rc.top1_bp == 0 && rc.num_objects_ge_1pct == 0 {
            return None;
        }
        let top1_pct = rc.top1_bp as f64 / 100.0;
        let top10_pct = rc.top10_bp as f64 / 100.0;
        let top_obj = r.top.biggest_objects.first();
        let detail = match top_obj.map(|o| match o.owner.as_deref() {
            Some(owner) => format!("`{}` (held by `{}`)", o.display_class, owner),
            None => format!("`{}`", o.display_class),
        }) {
            Some(name) => format!(
                "the single biggest object, {}, retains {:.1}% and the top 10 retain {:.1}% of the heap; {} objects each hold ≥1%.",
                name, top1_pct, top10_pct, rc.num_objects_ge_1pct,
            ),
            None => format!(
                "the single biggest object retains {:.1}% and the top 10 retain {:.1}% of the heap; {} objects each hold ≥1%.",
                top1_pct, top10_pct, rc.num_objects_ge_1pct,
            ),
        };
        let nav_class = top_obj
            .filter(|o| o.owner.is_none())
            .map(|o| o.display_class.clone());
        let mut sig = signal(
            "one-leak-or-many",
            TriageSeverity::Info,
            "One Leak or Many",
            detail,
            Some(("top-consumers", "Top Consumers")),
        );
        sig.nav_class = nav_class;
        Some(sig)
    }
}

// ── New rules ──────────────────────────────────────────────────────────────

/// Classloader leak. Reads `overview.duplicate_classes`.
struct ClassloaderLeak;
impl Rule for ClassloaderLeak {
    fn eval(&self, r: &Report) -> Option<TriageSignal> {
        let dup = r
            .overview
            .duplicate_classes
            .iter()
            .max_by_key(|d| d.total_retained)?;
        if dup.total_retained < 524_288 {
            return None;
        }
        if dup.loader_count < 5 {
            return Some(signal_cls(
                "classloader-leak",
                TriageSeverity::Info,
                "Class-Loader Reload (Low Count)",
                format!(
                    "`{}` is loaded by {} class loaders ({} retained) — possible reload, but count is low; investigate only if count grows.",
                    dup.pretty_class,
                    dup.loader_count,
                    format_bytes(dup.total_retained),
                ),
                Some(("duplicate-classes", "Duplicate Classes")),
                &dup.pretty_class,
            ));
        }
        Some(signal_cls(
            "classloader-leak",
            TriageSeverity::Warning,
            "Class-Loader Leak",
            format!(
                "`{}` is loaded by {} class loaders ({} retained) — classic redeploy/hot-reload leak; the old loader is still live. Check for static fields, ThreadLocals, or JNI globals referencing the old class.",
                dup.pretty_class,
                dup.loader_count,
                format_bytes(dup.total_retained),
            ),
            Some(("duplicate-classes", "Duplicate Classes")),
            &dup.pretty_class,
        ))
    }
}

/// ThreadLocal leak. Reads `leak_indicators.thread_local_null_key_count`.
struct ThreadLocalLeak;
impl Rule for ThreadLocalLeak {
    fn eval(&self, r: &Report) -> Option<TriageSignal> {
        let n = r.leak_indicators.thread_local_null_key_count;
        if n == 0 {
            return None;
        }
        Some(signal(
            "threadlocal-leak",
            TriageSeverity::Warning,
            "ThreadLocal Leak",
            format!(
                "{} ThreadLocalMap entries have a cleared key — the `ThreadLocal` object was GC'd but the value was never removed. Values accumulate until the thread terminates or `ThreadLocal.remove()` is called. Common in thread-pooled servers.",
                fmt_count(n),
            ),
            Some(("leak-indicators", "Leak Indicators")),
        ))
    }
}

/// Thread pinning. Reads `threads.threads` (retained + local_root_count).
struct ThreadPinning;
impl Rule for ThreadPinning {
    fn eval(&self, r: &Report) -> Option<TriageSignal> {
        let total = r.leaks.total_shallow;
        let t = r.threads.threads.iter().max_by_key(|t| t.retained)?;
        let share = pct_of(t.retained, total);
        if share < THREAD_PIN_PCT
            && !(t.local_root_count >= THREAD_PIN_LOCALS && share >= THREAD_PIN_LOCALS_MIN_PCT)
        {
            return None;
        }
        let who = t
            .name
            .as_deref()
            .or(t.class_name.as_deref())
            .unwrap_or("<unknown thread>");
        Some(signal(
            "thread-pinning",
            TriageSeverity::Warning,
            "Thread Pinning",
            format!(
                "thread `{}` retains {} ({:.1}% of heap) via {} thread-local GC root references — a running thread is pinning a disproportionate share of the heap. Inspect the thread's stack frames and ThreadLocal values.",
                who,
                format_bytes(t.retained),
                share,
                fmt_count(t.local_root_count),
            ),
            Some(("threads", "Threads")),
        ))
    }
}

/// Weak-ref escape. Reads `references.{soft,weak,phantom}.only_weakly_retained`.
struct WeakRefEscape;
impl Rule for WeakRefEscape {
    fn eval(&self, r: &Report) -> Option<TriageSignal> {
        let refs = &r.references;
        let only_weak_objects: u64 = [&refs.soft, &refs.weak, &refs.phantom]
            .into_iter()
            .flatten()
            .flat_map(|s| s.only_weakly_retained.iter())
            .map(|row| row.objects)
            .sum();
        let only_weak_retained: u64 = [&refs.soft, &refs.weak, &refs.phantom]
            .into_iter()
            .flatten()
            .flat_map(|s| s.only_weakly_retained.iter())
            .map(|row| row.retained)
            .sum();
        if only_weak_objects < WEAKREF_FLOOR && only_weak_retained < WEAKREF_BYTES_FLOOR {
            return None;
        }
        Some(signal(
            "weak-ref-escape",
            TriageSeverity::Info,
            "Only-Weakly Retained Objects",
            format!(
                "{} objects only weakly, softly, or phantom-retained, totaling {} — no strong path keeps them alive; GC will reclaim weak referents at the next collection and soft referents under memory pressure. If the count is unexpectedly high, check that no strong reference is silently held alongside the weak one.",
                fmt_count(only_weak_objects),
                format_bytes(only_weak_retained),
            ),
            Some(("references", "References")),
        ))
    }
}

/// Proxy/lambda bloat. Reads `leak_indicators.anonymous_class_count` and
/// `overview.classes_loaded`.
struct ProxyLambdaBloat;
impl Rule for ProxyLambdaBloat {
    fn eval(&self, r: &Report) -> Option<TriageSignal> {
        let anon = r.leak_indicators.anonymous_class_count;
        let loaded = r.overview.classes_loaded;
        if loaded < PROXY_MIN_CLASSES {
            return None;
        }
        let share = anon as f64 / loaded as f64 * 100.0;
        if share < PROXY_BLOAT_PCT {
            return None;
        }
        Some(signal(
            "proxy-lambda-bloat",
            TriageSeverity::Info,
            "Proxy/Lambda Bloat",
            format!(
                "{} of {} loaded classes ({:.1}%) are anonymous/generated (lambda/proxy) — possible class-loader churn; cache generated proxies or upgrade to newer Java where lambdas are method handles.",
                fmt_count(anon),
                fmt_count(loaded),
                share,
            ),
            Some(("leak-indicators", "Leak Indicators")),
        ))
    }
}

/// Off-heap (DirectByteBuffer). Reads `leak_indicators.direct_byte_buffer_capacity_sum`.
struct OffHeap;
impl Rule for OffHeap {
    fn eval(&self, r: &Report) -> Option<TriageSignal> {
        let cap = r.leak_indicators.direct_byte_buffer_capacity_sum;
        if cap < DBB_FLOOR_BYTES {
            return None;
        }
        Some(signal(
            "off-heap",
            TriageSeverity::Warning,
            "Off-Heap (DirectByteBuffer)",
            format!(
                "{} of native memory is held by live DirectByteBuffers — not reflected in the on-heap totals, but counts against process RSS and can trigger OS-level OOM.",
                format_bytes(cap),
            ),
            Some(("off-heap-nio", "Off-Heap NIO")),
        ))
    }
}

/// `unreachable_retained`, `unreachable_garbage_roots`.
struct GcWaste;
impl Rule for GcWaste {
    fn eval(&self, r: &Report) -> Option<TriageSignal> {
        let o = &r.overview;
        if o.heap_fragmentation_ratio < GC_WASTE_RATIO {
            return None;
        }
        let pct = o.heap_fragmentation_ratio * 100.0;
        let cluster = o
            .unreachable_garbage_roots
            .first()
            .map(|g| {
                format!(
                    " — largest garbage cluster rooted at `{}` ({})",
                    g.pretty_class,
                    format_bytes(g.retained),
                )
            })
            .unwrap_or_default();
        let size_desc =
            if o.unreachable_retained > o.unreachable_shallow + o.unreachable_shallow / 20 {
                format!(
                    "{} shallow, {} retained",
                    format_bytes(o.unreachable_shallow),
                    format_bytes(o.unreachable_retained)
                )
            } else {
                format_bytes(o.unreachable_shallow)
            };
        Some(signal(
            "gc-waste",
            TriageSeverity::Warning,
            "GC Waste",
            format!(
                "{:.1}% of the heap is unreachable ({}){}; the GC has not yet collected it. Trigger a full GC (`jcmd <pid> GC.run`) and re-dump — if the count drops sharply, the dump was taken mid-collection.",
                pct, size_desc, cluster,
            ),
            Some(("unreachable-objects", "Unreachable Objects")),
        ))
    }
}

/// Over-capacity collections (--collections only). Reads
/// `collections.collection_fill_ratio`. `tracked == 0` when --collections was off.
struct OverCapacityCollections;
impl Rule for OverCapacityCollections {
    fn eval(&self, r: &Report) -> Option<TriageSignal> {
        let total = r.leaks.total_shallow;
        let cfr = &r.collections.collection_fill_ratio;
        if cfr.tracked == 0 || total == 0 {
            return None;
        }
        let wasted: u64 = cfr
            .buckets
            .iter()
            .filter(|b| b.upper_ratio_bp <= OVERCAP_FILL_BP)
            .map(|b| b.wasted)
            .sum();
        if wasted as f64 / total as f64 * 100.0 < OVERCAP_WASTE_PCT {
            return None;
        }
        Some(signal(
            "over-capacity-collections",
            TriageSeverity::Info,
            "Over-Capacity Collections",
            format!(
                "{} wasted by under-filled collections (≤50% full across {} tracked) — for lists call `trimToSize()` after bulk population; for all types right-size initial capacity so the backing array is not over-allocated at construction.",
                format_bytes(wasted),
                fmt_count(cfr.tracked),
            ),
            Some(("collections", "Collections")),
        ))
    }
}

/// Constant-value arrays (--collections only). Reads
/// `collections.constant_primitive_arrays`. Empty rows when --collections was off.
struct ConstantValueArrays;
impl Rule for ConstantValueArrays {
    fn eval(&self, r: &Report) -> Option<TriageSignal> {
        let cpa = &r.collections.constant_primitive_arrays;
        if cpa.rows.is_empty() {
            return None;
        }
        let sum: u64 = cpa.rows.iter().map(|row| row.shallow).sum();
        if sum < CONSTARR_FLOOR {
            return None;
        }
        let big = cpa.rows.iter().max_by_key(|row| row.shallow)?;
        Some(signal_cls(
            "constant-value-arrays",
            TriageSeverity::Info,
            "Constant-Value Arrays",
            format!(
                "{} in single-value primitive arrays; biggest group `{}` × {} instances — replace duplicates with a shared constant (e.g. `static final byte[] EMPTY = new byte[0]`).",
                format_bytes(sum),
                big.array_class,
                fmt_count(big.objects),
            ),
            Some(("collections", "Collections")),
            &big.array_class,
        ))
    }
}

// ── New rules (batch 2) ───────────────────────────────────────────────────────

/// Object swarm. Reads `overview.histogram`. Fires when a single non-array class
/// has >= SWARM_FLOOR_INSTANCES live instances that are individually tiny but
/// collectively consume a large heap share — the signature of an unbounded
/// event/log/DTO accumulation.
struct ObjectSwarm;
impl Rule for ObjectSwarm {
    fn eval(&self, r: &Report) -> Option<TriageSignal> {
        let total = r.overview.total_shallow;
        let row = r
            .overview
            .histogram
            .iter()
            .filter(|h| {
                !h.pretty_class.ends_with("[]")
                    && h.instances >= SWARM_FLOOR_INSTANCES
                    && (h.instances == 0 || h.shallow / h.instances <= SWARM_MAX_INSTANCE_BYTES)
            })
            .max_by_key(|h| h.shallow)?;
        if pct_of(row.shallow, total) < SWARM_PCT {
            return None;
        }
        Some(signal_cls(
            "object-swarm",
            TriageSeverity::Warning,
            "Object Swarm",
            format!(
                "{} live `{}` instances ({} shallow, {:.1}% of heap) — many tiny objects accumulating; check for an unbounded queue, growing log buffer, or DTO/event accumulation. Either cap the collection or process and discard entries on-the-fly.",
                fmt_count(row.instances),
                row.pretty_class,
                format_bytes(row.shallow),
                pct_of(row.shallow, total),
            ),
            Some(("system-overview", "System Overview")),
            &row.pretty_class,
        ))
    }
}

/// Boxed-primitive bloat. Reads `overview.histogram`. Fires when the total
/// live count of `java.lang.{Integer,Long,Double,…}` wrapper objects is very
/// high — often a Map/List that should use a primitive-specialized collection.
struct BoxedPrimitiveBloat;
impl Rule for BoxedPrimitiveBloat {
    fn eval(&self, r: &Report) -> Option<TriageSignal> {
        const BOXED: &[&str] = &[
            "java.lang.Integer",
            "java.lang.Long",
            "java.lang.Double",
            "java.lang.Float",
            "java.lang.Short",
            "java.lang.Byte",
            "java.lang.Character",
            "java.lang.Boolean",
        ];
        let total = r.overview.total_shallow;
        let (instances, shallow, worst_class) = r
            .overview
            .histogram
            .iter()
            .filter(|h| BOXED.iter().any(|b| h.pretty_class == *b))
            .fold((0u64, 0u64, ""), |(inst, sh, worst), h| {
                let new_worst = if h.instances > inst || worst.is_empty() {
                    h.pretty_class.as_str()
                } else {
                    worst
                };
                (inst + h.instances, sh + h.shallow, new_worst)
            });
        if instances < BOXED_FLOOR_INSTANCES && pct_of(shallow, total) < BOXED_PCT {
            return None;
        }
        Some(signal(
            "boxed-primitive-bloat",
            TriageSeverity::Info,
            "Boxed-Primitive Bloat",
            format!(
                "{} boxed-primitive objects ({} shallow, led by `{}`) — consider primitive-specialized collections (e.g. Eclipse Collections, Koloboke).",
                fmt_count(instances),
                format_bytes(shallow),
                worst_class,
            ),
            Some(("boxed-numbers", "Boxed Numbers")),
        ))
    }
}

/// Classloader explosion. Reads `overview.classloaders_loaded`. Fires when the
/// live ClassLoader count is abnormally high — dynamic scripting (Groovy/JSP),
/// repeated redeployments, or proxy generators leaking loaders.
struct ClassloaderExplosion;
impl Rule for ClassloaderExplosion {
    fn eval(&self, r: &Report) -> Option<TriageSignal> {
        let n = r.overview.classloaders_loaded;
        if n < CLASSLOADER_EXPLOSION_FLOOR {
            return None;
        }
        Some(signal(
            "classloader-explosion",
            TriageSeverity::Warning,
            "Class-Loader Explosion",
            format!(
                "{} live class-loader instances — abnormally high; typical apps use tens. Likely dynamic-class or redeploy leak: check for Groovy/JSP script-engine leaks, CGLIB proxy caching, or undischarged application-server contexts.",
                fmt_count(n),
            ),
            Some(("system-overview", "System Overview")),
        ))
    }
}

/// Thread swarm. Reads `threads.threads`. Fires when the live thread count is
/// abnormally high — unbounded thread creation or a leaking
/// ExecutorService/ThreadPoolExecutor per request. The aggregate-share path is
/// intentionally omitted: a high aggregate caused by *one* dominant thread is
/// already surfaced by ThreadPinning; thread-swarm targets *count*.
struct ThreadSwarm;
impl Rule for ThreadSwarm {
    fn eval(&self, r: &Report) -> Option<TriageSignal> {
        let threads = &r.threads.threads;
        let count = threads.len();
        if count < THREAD_SWARM_FLOOR {
            return None;
        }
        let aggregate_retained: u64 = threads.iter().map(|t| t.retained).sum();
        Some(signal(
            "thread-swarm",
            TriageSeverity::Warning,
            "Thread Swarm",
            format!(
                "{} live threads retaining {} in aggregate — likely unbounded thread creation or a leaking thread pool. Ensure ExecutorServices are shut down when no longer needed; on Java 21+ prefer virtual threads for I/O-bound workloads.",
                fmt_count(count as u64),
                format_bytes(aggregate_retained),
            ),
            Some(("threads", "Threads")),
        ))
    }
}

/// Duplicate strings (--find-duplicates only). Reads
/// `overview.duplicate_strings.{approx_wasted_bytes, top_duplicated}`.
/// Silent when `--find-duplicates` was not passed.
struct DuplicateStrings;
impl Rule for DuplicateStrings {
    fn eval(&self, r: &Report) -> Option<TriageSignal> {
        let ds = r.overview.duplicate_strings.as_ref()?;
        let total = r.overview.total_shallow;
        if ds.approx_wasted_bytes < DUP_STRINGS_FLOOR_BYTES
            && pct_of(ds.approx_wasted_bytes, total) < DUP_STRINGS_PCT
        {
            return None;
        }
        let top = ds.top_duplicated.first();
        let example = top
            .map(|t| format!("; `\"{}\"` repeated {}×", t.text, fmt_count(t.count),))
            .unwrap_or_default();
        Some(signal(
            "duplicate-strings",
            TriageSeverity::Info,
            "Duplicate Strings",
            format!(
                "~{} wasted by {} duplicated String values ({} total instances){}. Enable JVM string deduplication (`-XX:+UseStringDeduplication` with G1GC), or intern/pool strings at creation time.",
                format_bytes(ds.approx_wasted_bytes),
                fmt_count(ds.duplicated_values),
                fmt_count(ds.total_string_instances),
                example,
            ),
            Some(("duplicate-strings", "Duplicate Strings")),
        ))
    }
}

/// Char-array slack (--find-duplicates only). Reads
/// `overview.duplicate_strings.char_array_waste`. Silent when `--find-duplicates`
/// was not passed or no char-array waste was computed.
struct CharArraySlack;
impl Rule for CharArraySlack {
    fn eval(&self, r: &Report) -> Option<TriageSignal> {
        let caw = r
            .overview
            .duplicate_strings
            .as_ref()
            .and_then(|ds| ds.char_array_waste.as_ref())?;
        if caw.total_wasted_bytes < CHAR_SLACK_FLOOR_BYTES
            || caw.wasteful_arrays < CHAR_SLACK_MIN_ARRAYS
        {
            return None;
        }
        Some(signal(
            "char-array-slack",
            TriageSeverity::Info,
            "Char-Array Slack",
            format!(
                "~{} slack in {} over-allocated char[]/byte[] String backing arrays — common from pre-sized `StringBuilder` allocations that are never fully filled, or `String(byte[], offset, length)` where the source array is larger than the result. Use `new String(str)` to copy-compact, or size StringBuilder capacity to the expected output length.",
                format_bytes(caw.total_wasted_bytes),
                fmt_count(caw.wasteful_arrays),
            ),
            Some(("duplicate-strings", "Duplicate Strings")),
        ))
    }
}

/// Large unbounded collection (--collections only). Reads `biggest_collections`.
/// Fires when a single collection instance has an extreme element count or
/// dominates the heap — the archetypal static/unbounded cache.
struct LargeUnboundedCollection;
impl Rule for LargeUnboundedCollection {
    fn eval(&self, r: &Report) -> Option<TriageSignal> {
        let bc = r.biggest_collections.as_ref()?;
        let row = bc.combined.iter().max_by_key(|c| c.elements)?;
        if row.elements < UNBOUNDED_COLL_ELEMENTS {
            // Also check by retained share when available.
            let retained_ok = row
                .retained
                .map(|ret| pct_of(ret, r.leaks.total_shallow) >= UNBOUNDED_COLL_PCT)
                .unwrap_or(false);
            if !retained_ok {
                return None;
            }
        }
        let retained_str = row
            .retained
            .map(|ret| format!(", retaining {}", format_bytes(ret)))
            .unwrap_or_default();
        let owner_str = row
            .owner
            .as_deref()
            .map(|o| format!(" (held by `{}`)", o))
            .unwrap_or_default();
        Some(signal_cls(
            "large-unbounded-collection",
            TriageSeverity::Warning,
            "Large Unbounded Collection",
            format!(
                "one `{}` holds {} elements{}{} — likely a static or unbounded cache that never evicts. Add a maximum-size eviction policy (e.g. Caffeine/Guava `maximumSize`, `LinkedHashMap` LRU override, or `removeEldestEntry`).",
                row.container_class,
                fmt_count(row.elements),
                retained_str,
                owner_str,
            ),
            Some(("biggest-collections", "Biggest Collections")),
            &row.container_class,
        ))
    }
}

// ── New rules (batch 3) ───────────────────────────────────────────────────────

/// Finalizer queue backlog. Reads `overview.histogram` for `java.lang.ref.Finalizer`.
/// Fires when the finalizer thread cannot drain the queue as fast as objects are
/// promoted to it.
struct FinalizerQueueBacklog;
impl Rule for FinalizerQueueBacklog {
    fn eval(&self, r: &Report) -> Option<TriageSignal> {
        let row = r
            .overview
            .histogram
            .iter()
            .find(|h| h.pretty_class == "java.lang.ref.Finalizer")?;
        if row.instances < FINALIZER_FLOOR {
            return None;
        }
        Some(signal(
            "finalizer-queue-backlog",
            TriageSeverity::Warning,
            "Finalizer Queue Backlog",
            format!(
                "{} live `java.lang.ref.Finalizer` instances — the finalizer thread is falling behind; objects with `finalize()` (e.g. `Deflater`, JDBC connections) accumulate faster than they are drained. Prefer explicit `close()` over relying on `finalize()`.",
                fmt_count(row.instances),
            ),
            Some(("system-overview", "System Overview")),
        ))
    }
}

/// Metaspace pressure. Reads `overview.classes_loaded`. Fires when the absolute
/// loaded-class count is abnormally high, indicating CGLIB/Byte Buddy/Groovy
/// proxy generation without caching that will exhaust Metaspace.
struct MetaspacePressure;
impl Rule for MetaspacePressure {
    fn eval(&self, r: &Report) -> Option<TriageSignal> {
        let n = r.overview.classes_loaded;
        if n < METASPACE_CLASS_FLOOR {
            return None;
        }
        Some(signal(
            "metaspace-pressure",
            TriageSeverity::Warning,
            "Metaspace Pressure",
            format!(
                "{} classes loaded — far above normal; class metadata is likely exhausting Metaspace. Typical cause: CGLIB/Byte Buddy/Groovy proxy generation without caching. Add `-XX:MaxMetaspaceSize` to cap growth, enable proxy caching, and look for repeated `defineClass` call sites.",
                fmt_count(n),
            ),
            Some(("system-overview", "System Overview")),
        ))
    }
}

/// Cached reflection metadata. Reads `overview.histogram` for
/// `java.lang.reflect.{Method,Field,Constructor}`. Fires when framework
/// reflective caches accumulate unbounded reflection objects.
struct CachedReflectionMetadata;
impl Rule for CachedReflectionMetadata {
    fn eval(&self, r: &Report) -> Option<TriageSignal> {
        const REFLECT_CLASSES: &[&str] = &[
            "java.lang.reflect.Method",
            "java.lang.reflect.Field",
            "java.lang.reflect.Constructor",
        ];
        let total: u64 = r
            .overview
            .histogram
            .iter()
            .filter(|h| REFLECT_CLASSES.iter().any(|&c| h.pretty_class == c))
            .map(|h| h.instances)
            .sum();
        if total < REFLECT_FLOOR {
            return None;
        }
        Some(signal(
            "cached-reflection-metadata",
            TriageSeverity::Info,
            "Cached Reflection Metadata",
            format!(
                "{} live `java.lang.reflect.{{Method,Field,Constructor}}` objects — framework reflection caches are unbounded (typically Spring/Hibernate accumulating per scanned class). Check for uncapped `ReflectionUtils` caches or scanner loops calling `getDeclaredMethods()` without caching the result.",
                fmt_count(total),
            ),
            Some(("system-overview", "System Overview")),
        ))
    }
}

/// JNI global-reference leak. Reads `overview.gc_roots_by_type` (count) and
/// `overview.gc_roots_retained_by_type` (retained share). Fires when native
/// code accumulates JNI global references without releasing them.
struct JniGlobalRefLeak;
impl Rule for JniGlobalRefLeak {
    fn eval(&self, r: &Report) -> Option<TriageSignal> {
        let count = r
            .overview
            .gc_roots_by_type
            .iter()
            .find(|row| row.root_type == "JNI Global")
            .map(|row| row.count)
            .unwrap_or(0);
        if count < JNI_GLOBAL_FLOOR {
            return None;
        }
        let total = r.overview.total_shallow;
        let retained = r
            .overview
            .gc_roots_retained_by_type
            .iter()
            .find(|row| row.root_type == "JNI Global")
            .map(|row| row.retained)
            .unwrap_or(0);
        if pct_of(retained, total) < JNI_GLOBAL_RETAINED_PCT {
            return None;
        }
        Some(signal(
            "jni-global-ref-leak",
            TriageSeverity::Warning,
            "JNI Global-Reference Leak",
            format!(
                "{} JNI Global roots retaining {} ({:.1}% of heap) — native code is accumulating global references without releasing them; audit `JNI_DeleteGlobalRef` call sites.",
                fmt_count(count),
                format_bytes(retained),
                pct_of(retained, total),
            ),
            Some(("system-overview", "System Overview")),
        ))
    }
}

/// Heap composition skew. Reads `overview.heap_composition.by_kind`. Fires when
/// a single kind (e.g. primitive arrays) dominates the heap, pointing at
/// bulk-data caches, NIO buffers, or sparse object-array structures.
struct HeapCompositionSkew;
impl Rule for HeapCompositionSkew {
    fn eval(&self, r: &Report) -> Option<TriageSignal> {
        let total = r.overview.total_shallow;
        if total == 0 {
            return None;
        }
        let dominant = r
            .overview
            .heap_composition
            .by_kind
            .iter()
            .max_by_key(|k| k.shallow_heap)?;
        let pct = pct_of(dominant.shallow_heap, total);
        if pct < HEAP_SKEW_PCT {
            return None;
        }
        let hint = match dominant.kind.as_str() {
            "Primitive Arrays" => {
                "check for bulk-data buffers (NIO, image, audio) or oversized backing stores"
            }
            "Instances" => "too many small objects — see Object Swarm or Boxed-Primitive Bloat",
            "Object Arrays" => {
                "sparse arrays or container backing stores; check collection fill ratios"
            }
            "Class Objects" => {
                "many dynamically generated classes — see Class-Loader Explosion or Metaspace Pressure"
            }
            _ => "inspect the Class Histogram for the dominant contributors",
        };
        Some(signal(
            "heap-composition-skew",
            TriageSeverity::Info,
            "Heap Composition Skew",
            format!(
                "{} account for {:.1}% of reachable heap — unusually skewed; {}.",
                dominant.kind, pct, hint,
            ),
            Some(("system-overview", "System Overview")),
        ))
    }
}

/// Static-field anchor. Reads `leaks.suspects`. Fires when the top suspect is
/// anchored by a `Sticky Class` GC root (i.e. a static field) and retains a
/// large heap share — classic "static cache that never evicts".
struct StaticFieldAnchor;
impl Rule for StaticFieldAnchor {
    fn eval(&self, r: &Report) -> Option<TriageSignal> {
        let s = r.leaks.suspects.first()?;
        if s.root_type_label != "Sticky Class" {
            return None;
        }
        let total = r.leaks.total_shallow;
        let pct = pct_of(s.retained, total);
        if pct < STATIC_ANCHOR_PCT {
            return None;
        }
        Some(signal_cls(
            "static-field-anchor",
            TriageSeverity::Warning,
            "Static-Field Anchor",
            format!(
                "`{}` is anchored via a static field (`Sticky Class` root) and retains {} ({:.1}% of heap) — the object lives for the class-loader lifetime; add eviction, null out the field after use, or replace with a `WeakReference` if the data should be reclaimable.",
                s.pretty_class,
                format_bytes(s.retained),
                pct,
            ),
            Some(("leak-suspects", "Leak Suspects")),
            &s.pretty_class,
        ))
    }
}

/// Session / request-scope leak. Reads `overview.histogram`. Fires when a class
/// whose name suggests session or request scope accumulates in very large numbers
/// — sessions that are never invalidated or request contexts that are never freed.
struct SessionScopeLeak;
impl Rule for SessionScopeLeak {
    fn eval(&self, r: &Report) -> Option<TriageSignal> {
        let row = r
            .overview
            .histogram
            .iter()
            .filter(|h| {
                let c = &h.pretty_class;
                (c.contains("Session") || c.contains("session"))
                    && !c.contains("[]")
                    && h.instances >= SESSION_FLOOR
            })
            .max_by_key(|h| h.instances)?;
        Some(signal_cls(
            "session-scope-leak",
            TriageSeverity::Warning,
            "Session-Scope Leak",
            format!(
                "{} live `{}` instances — session objects accumulating without invalidation; check that sessions are expired/invalidated on logout and that an idle-timeout is configured.",
                fmt_count(row.instances),
                row.pretty_class,
            ),
            Some(("system-overview", "System Overview")),
            &row.pretty_class,
        ))
    }
}

/// Connection / socket leak. Reads `overview.histogram`. Fires when a class
/// whose name suggests a connection or socket accumulates beyond a reasonable
/// pool size — connections acquired but never returned or closed.
struct ConnectionLeak;
impl Rule for ConnectionLeak {
    fn eval(&self, r: &Report) -> Option<TriageSignal> {
        // Include Connection/Socket/Statement/ResultSet; exclude weak-ref wrappers and arrays.
        let row = r
            .overview
            .histogram
            .iter()
            .filter(|h| {
                let c = &h.pretty_class;
                !c.ends_with("[]")
                    && !c.contains("Weak")
                    && !c.contains("Reference")
                    && (c.contains("Connection") || c.contains("Socket"))
                    && h.instances >= CONNECTION_FLOOR
            })
            .max_by_key(|h| h.instances)?;
        Some(signal_cls(
            "connection-leak",
            TriageSeverity::Warning,
            "Connection / Socket Leak",
            format!(
                "{} live `{}` objects — exceeds any reasonable pool or connection limit. Wrap acquisitions in try-with-resources, or enable connection-pool leak detection (e.g. HikariCP `leakDetectionThreshold`, c3p0 `unreturnedConnectionTimeout`).",
                fmt_count(row.instances),
                row.pretty_class,
            ),
            Some(("system-overview", "System Overview")),
            &row.pretty_class,
        ))
    }
}

/// Event-listener accumulation. Reads `overview.histogram`. Fires when a class
/// whose name suggests an event listener or observer accumulates in large numbers
/// — listeners registered to a long-lived publisher but never unregistered.
struct EventListenerAccumulation;
impl Rule for EventListenerAccumulation {
    fn eval(&self, r: &Report) -> Option<TriageSignal> {
        let row = r
            .overview
            .histogram
            .iter()
            .filter(|h| {
                let c = &h.pretty_class;
                !c.ends_with("[]")
                    && (c.contains("Listener")
                        || c.contains("Observer")
                        || c.contains("Subscriber"))
                    && h.instances >= LISTENER_FLOOR
            })
            .max_by_key(|h| h.instances)?;
        Some(signal_cls(
            "event-listener-accumulation",
            TriageSeverity::Warning,
            "Event-Listener Accumulation",
            format!(
                "{} live `{}` instances — listeners accumulating without removal; call `removeListener()` / `unsubscribe()` when the component is disposed, or use weak-reference listener registries.",
                fmt_count(row.instances),
                row.pretty_class,
            ),
            Some(("system-overview", "System Overview")),
            &row.pretty_class,
        ))
    }
}

/// Parser-output accumulation. Reads `overview.histogram`. Fires when classes
/// from XML/JSON parser output packages accumulate in large numbers — parsed
/// documents retained in caches instead of being discarded after processing.
struct ParserOutputAccumulation;
impl Rule for ParserOutputAccumulation {
    fn eval(&self, r: &Report) -> Option<TriageSignal> {
        const PARSER_PKGS: &[&str] = &[
            "org.w3c.dom.",
            "com.fasterxml.jackson.",
            "com.google.gson.",
            "org.dom4j.",
            "org.jdom.",
            "nu.xom.",
            "javax.xml.",
            "jakarta.xml.",
        ];
        let row = r
            .overview
            .histogram
            .iter()
            .filter(|h| {
                !h.pretty_class.ends_with("[]")
                    && PARSER_PKGS
                        .iter()
                        .any(|pkg| h.pretty_class.starts_with(pkg))
                    && h.instances >= PARSER_FLOOR
            })
            .max_by_key(|h| h.instances)?;
        Some(signal_cls(
            "parser-output-accumulation",
            TriageSeverity::Info,
            "Parser-Output Accumulation",
            format!(
                "{} live `{}` instances — XML/JSON parse results are accumulating; discard documents after processing, or use a streaming parser (SAX/StAX/Jackson streaming) instead of building a full in-memory tree.",
                fmt_count(row.instances),
                row.pretty_class,
            ),
            Some(("system-overview", "System Overview")),
            &row.pretty_class,
        ))
    }
}

/// Interned-string bloat. Reads `overview.histogram` (String count) and
/// `overview.gc_roots_by_type` (JNI Global count). Fires when both are elevated,
/// suggesting `String.intern()` is called at scale on dynamically generated values,
/// causing the intern table to grow without bound.
struct InternedStringBloat;
impl Rule for InternedStringBloat {
    fn eval(&self, r: &Report) -> Option<TriageSignal> {
        let string_count = r
            .overview
            .histogram
            .iter()
            .find(|h| h.pretty_class == "java.lang.String")
            .map(|h| h.instances)
            .unwrap_or(0);
        if string_count < INTERNED_STRING_FLOOR {
            return None;
        }
        let jni_global_count = r
            .overview
            .gc_roots_by_type
            .iter()
            .find(|row| row.root_type == "JNI Global")
            .map(|row| row.count)
            .unwrap_or(0);
        if jni_global_count < INTERNED_JNI_FLOOR {
            return None;
        }
        Some(signal(
            "interned-string-bloat",
            TriageSeverity::Warning,
            "Interned-String Bloat",
            format!(
                "{} live `java.lang.String` instances with {} JNI Global roots — the intern table may be growing without bound from calls to `String.intern()` on dynamic or user-supplied values. Replace with a bounded cache (e.g. Guava `Interner` or `ConcurrentHashMap`) and avoid `intern()` on strings that are not truly constants.",
                fmt_count(string_count),
                fmt_count(jni_global_count),
            ),
            Some(("system-overview", "System Overview")),
        ))
    }
}

/// Sparse object arrays (--collections only). Reads `collections.array_fill_ratio`.
/// Fires when many tracked object arrays are very sparsely populated, wasting
/// memory on null slots — common with multi-dimensional or pre-sized sparse arrays.
struct SparseObjectArrays;
impl Rule for SparseObjectArrays {
    fn eval(&self, r: &Report) -> Option<TriageSignal> {
        let afr = &r.collections.array_fill_ratio;
        if afr.tracked < SPARSE_ARRAY_MIN_TRACKED {
            return None;
        }
        let total_heap = r.leaks.total_shallow;
        // Sum wasted bytes across buckets with fill <= SPARSE_ARRAY_FILL_BP.
        let (sparse_objects, wasted): (u64, u64) = afr
            .buckets
            .iter()
            .filter(|b| b.upper_ratio_bp <= SPARSE_ARRAY_FILL_BP)
            .fold((0, 0), |(obj, w), b| (obj + b.objects, w + b.wasted));
        if sparse_objects < SPARSE_ARRAY_MIN_TRACKED
            || pct_of(wasted, total_heap) < SPARSE_ARRAY_WASTED_PCT
        {
            return None;
        }
        Some(signal(
            "sparse-object-arrays",
            TriageSeverity::Info,
            "Sparse Object Arrays",
            format!(
                "{} object arrays are ≤{}% full ({} wasted on null slots) — sparse or multi-dimensional array structures consuming excess memory. Replace with a `HashMap` / `SparseArray`, a `List` that grows on demand, or a dedicated sparse-matrix library.",
                fmt_count(sparse_objects),
                SPARSE_ARRAY_FILL_BP / 100,
                format_bytes(wasted),
            ),
            Some(("collections", "Collections")),
        ))
    }
}

// ── Batch 4: JXRay-inspired + queued rules ────────────────────────────────────

/// Big-drop concentration. Reads `dominator_analysis.big_drops` and
/// `overview.total_shallow`. Always-on. Fires when the top dominator-tree node
/// drops at least BIG_DROP_PCT of the heap AND at least BIG_DROP_FLOOR bytes —
/// a single object is acting as a giant memory bucket.
struct BigDropConcentration;
impl Rule for BigDropConcentration {
    fn eval(&self, r: &Report) -> Option<TriageSignal> {
        let total = r.overview.total_shallow;
        let row = r.dominator_analysis.big_drops.rows.first()?;
        if row.drop_bytes < BIG_DROP_FLOOR {
            return None;
        }
        let pct = pct_of(row.drop_bytes, total);
        if pct < BIG_DROP_PCT {
            return None;
        }
        Some(signal_cls(
            "big-drop-concentration",
            TriageSeverity::Critical,
            "Dominator-Tree Big Drop",
            format!(
                "`{}` is the single largest memory bucket: {:.1}% ({}) of the heap \
                 drops here in the dominator tree — every path from a GC root to those objects \
                 passes through this one node. Follow the retaining chain to find the GC root that keeps it alive.",
                row.display_class,
                pct,
                format_bytes(row.drop_bytes),
            ),
            Some(("dominator-analysis", "Dominator Analysis")),
            &row.display_class,
        ))
    }
}

/// Fixed per-object overhead. Reads `overview.{total_objects, total_shallow,
/// identifier_size_bits, compressed_oops}`. Always-on. Fires when the aggregate
/// 12-or-16-byte object header cost exceeds HEADER_OVERHEAD_PCT of the heap —
/// the signature of a design using millions of tiny wrapper objects.
struct FixedPerObjectOverhead;
impl Rule for FixedPerObjectOverhead {
    fn eval(&self, r: &Report) -> Option<TriageSignal> {
        let total = r.overview.total_shallow;
        if total == 0 {
            return None;
        }
        // Header = 12 bytes for compressed-oops or 32-bit JVM, 16 bytes otherwise.
        let header_bytes: u64 = if r.overview.identifier_size_bits == 32
            || r.overview.compressed_oops.unwrap_or(true)
        {
            12
        } else {
            16
        };
        let overhead = r.overview.total_objects.saturating_mul(header_bytes);
        let pct = overhead as f64 / total as f64 * 100.0;
        if pct < HEADER_OVERHEAD_PCT {
            return None;
        }
        Some(signal(
            "fixed-per-object-overhead",
            TriageSeverity::Warning,
            "Fixed per-Object Header Overhead",
            format!(
                "{} ({:.1}% of heap) consumed by JVM object headers alone \
                 ({} objects × {} B each) — consider replacing wrapper objects with \
                 primitive arrays, off-heap buffers, or primitive-specialized collections.",
                format_bytes(overhead),
                pct,
                fmt_count(r.overview.total_objects),
                header_bytes,
            ),
            Some(("object-header-overhead", "Object Header Overhead")),
        ))
    }
}

/// Hash-map collision hotspot. Reads `collections.map_collision_ratio`. Always-on
/// (the collision ratio is computed in the always-on field-decode pass). Fires
/// when a significant fraction of tracked maps are over-full (load > 90%), which
/// causes O(n) key-lookup chains and inflates retained memory.
struct HashCollisionHotspot;
impl Rule for HashCollisionHotspot {
    fn eval(&self, r: &Report) -> Option<TriageSignal> {
        let mcr = &r.collections.map_collision_ratio;
        if mcr.tracked < COLLISION_MIN_TRACKED {
            return None;
        }
        let hot: u64 = mcr
            .buckets
            .iter()
            .filter(|b| b.lower_ratio_bp >= COLLISION_HIGH_BP)
            .map(|b| b.objects)
            .sum();
        if hot == 0 {
            return None;
        }
        let pct = pct_of(hot, mcr.tracked);
        Some(signal(
            "hash-collision-hotspot",
            TriageSeverity::Warning,
            "Hash-Map Collision Hotspot",
            format!(
                "{} of {} tracked maps ({:.1}%) have a load factor > {}% — \
                 over-packed hash tables cause long collision chains and degrade \
                 lookup performance. Increase initial capacity or lower the load factor \
                 (pass `initialCapacity` and `loadFactor` to the constructor, default is 0.75).",
                fmt_count(hot),
                fmt_count(mcr.tracked),
                pct,
                COLLISION_HIGH_BP / 100,
            ),
            Some(("collections", "Collections")),
        ))
    }
}

/// Empty-collection cemetery. Reads `collections.collections_by_size`. Always-on.
/// Fires when most (or very many) tracked collections are empty — allocated but
/// never populated, wasting object-header overhead at scale.
struct EmptyCollectionCemetery;
impl Rule for EmptyCollectionCemetery {
    fn eval(&self, r: &Report) -> Option<TriageSignal> {
        let cbs = &r.collections.collections_by_size;
        if cbs.tracked == 0 {
            return None;
        }
        let share_pct = pct_of(cbs.empty_count, cbs.tracked);
        if share_pct < EMPTY_COLL_SHARE_PCT && cbs.empty_count < EMPTY_COLL_FLOOR {
            return None;
        }
        Some(signal(
            "empty-collection-cemetery",
            TriageSeverity::Info,
            "Empty-Collection Cemetery",
            format!(
                "{} of {} tracked collections ({:.1}%) are empty — \
                 pre-allocated but never populated containers waste object-header \
                 overhead at scale. Use lazy initialization (allocate only when the \
                 first element is added) or return `Collections.emptyList()` / \
                 `List.of()` sentinels for the read-only empty case.",
                fmt_count(cbs.empty_count),
                fmt_count(cbs.tracked),
                share_pct,
            ),
            Some(("collections", "Collections")),
        ))
    }
}

/// Oversized primitive array. Reads `collections.top_prim_arrays.top_individual`
/// and `overview.total_shallow`. Always-on (top_prim_arrays is always computed).
/// Fires when a single primitive array is individually >= OVERSIZED_PRIM_ARRAY_PCT
/// of the heap AND >= OVERSIZED_PRIM_ARRAY_FLOOR bytes.
struct OversizedPrimArray;
impl Rule for OversizedPrimArray {
    fn eval(&self, r: &Report) -> Option<TriageSignal> {
        let total = r.overview.total_shallow;
        let row = r.collections.top_prim_arrays.top_individual.first()?;
        if row.shallow < OVERSIZED_PRIM_ARRAY_FLOOR {
            return None;
        }
        let pct = pct_of(row.shallow, total);
        if pct < OVERSIZED_PRIM_ARRAY_PCT {
            return None;
        }
        let owner_clause = match &row.owner {
            Some(o) => format!(" held by `{o}`"),
            None => String::new(),
        };
        Some(signal_cls(
            "oversized-prim-array",
            TriageSeverity::Warning,
            "Oversized Primitive Array",
            format!(
                "A single `{}` ({} elements, {}){} accounts for {:.1}% of the heap — \
                 consider chunking, memory-mapping, or off-heap storage.",
                row.array_class,
                fmt_count(row.length),
                format_bytes(row.shallow),
                owner_clause,
                pct,
            ),
            Some(("arrays-by-size", "Arrays by Size")),
            &row.array_class,
        ))
    }
}

/// Duplicate primitive arrays. Reads `overview.duplicate_prim_arrays`
/// (populated only when `--find-duplicates` is active). Fires when content-identical
/// prim arrays waste at least DUP_PRIM_ARRAYS_PCT of the heap or DUP_PRIM_ARRAYS_FLOOR
/// bytes — arrays sharing the same payload could be deduplicated or interned.
struct DuplicatePrimArrays;
impl Rule for DuplicatePrimArrays {
    fn eval(&self, r: &Report) -> Option<TriageSignal> {
        let dpa = r.overview.duplicate_prim_arrays.as_ref()?;
        let wasted = dpa.total_wasted_bytes;
        if wasted == 0 {
            return None;
        }
        let total = r.overview.total_shallow;
        if wasted < DUP_PRIM_ARRAYS_FLOOR && pct_of(wasted, total) < DUP_PRIM_ARRAYS_PCT {
            return None;
        }
        Some(signal(
            "dup-prim-arrays",
            TriageSeverity::Warning,
            "Duplicate Primitive Arrays",
            format!(
                "{} ({:.1}% of heap) wasted by content-identical primitive arrays — \
                 multiple copies of the same byte[]/int[]/etc. payload could be \
                 deduplicated or replaced with a shared constant.",
                format_bytes(wasted),
                pct_of(wasted, total),
            ),
            Some(("duplicate-prim-arrays", "Duplicate Primitive Arrays")),
        ))
    }
}

/// Deep retention chain. Reads `dominator_analysis.longest_chain_depth`.
/// Always-on. Fires when the deepest dominator chain exceeds DEEP_CHAIN_DEPTH
/// hops — a linked-list-shaped heap where objects are chained one-by-one rather
/// than held in a flat container. Typical in unbounded `LinkedList`/`Deque`
/// accumulation or recursive data structures that are never cleared.
struct DeepRetentionChain;
impl Rule for DeepRetentionChain {
    fn eval(&self, r: &Report) -> Option<TriageSignal> {
        let depth = r.dominator_analysis.longest_chain_depth;
        if depth < DEEP_CHAIN_DEPTH {
            return None;
        }
        Some(signal(
            "deep-retention-chain",
            TriageSeverity::Warning,
            "Deep Retention Chain (Linked-List Shape)",
            format!(
                "The dominator tree's longest chain is {} hops — this is a linked-list-shaped \
                 heap. Memory is being accumulated object-by-object in an unbounded chain \
                 (e.g. `LinkedList`, `ArrayDeque` backed by a linked structure, or a recursive \
                 data structure). Consider replacing with an array-backed container \
                 (`ArrayList`, `ArrayDeque`) or bounding growth.",
                fmt_count(depth as u64),
            ),
            Some(("dominator-analysis", "Dominator Analysis")),
        ))
    }
}

/// Framework-retained-heap leak. Reads `framework_analysis`. Fires when any
/// detected framework retains at least FRAMEWORK_RETAINED_FLOOR bytes AND
/// at least FRAMEWORK_RETAINED_PCT of the heap. A sentinel class holding far
/// more than expected indicates retained application contexts, open sessions,
/// or leaked prototype beans.
struct FrameworkLeak;
impl Rule for FrameworkLeak {
    fn eval(&self, r: &Report) -> Option<TriageSignal> {
        let total = r.overview.total_shallow;
        if total == 0 {
            return None;
        }
        // Pick the framework with the most retained heap as the headline.
        let fa = r
            .framework_analysis
            .iter()
            .max_by_key(|f| f.total_retained)?;
        if fa.total_retained < FRAMEWORK_RETAINED_FLOOR {
            return None;
        }
        let pct = pct_of(fa.total_retained, total);
        if pct < FRAMEWORK_RETAINED_PCT {
            return None;
        }
        let advice = if fa.framework.contains("Spring") {
            "Check for multiple retained `ApplicationContext` instances, prototype-scoped \
             beans stored in singletons, or `@Autowired` fields captured in closures."
        } else if fa.framework.contains("Hibernate") || fa.framework.contains("JPA") {
            "Check for unclosed `Session`/`EntityManager` instances, first-level caches \
             grown unboundedly, or collections marked `FetchType.EAGER` in bulk queries."
        } else {
            "Check for retained context/session/factory instances that were not closed."
        };
        Some(signal(
            "framework-leak",
            TriageSeverity::Warning,
            "Framework Retained-Heap Leak",
            format!(
                "{} objects retain {} ({:.1}% of heap) via `{}` sentinel instances — {}",
                fmt_count(fa.instance_count as u64),
                format_bytes(fa.total_retained),
                pct,
                fa.framework,
                advice,
            ),
            Some(("system-overview", "System Overview")),
        ))
    }
}

/// Blocked/waiting thread concentration. Reads `threads.threads` and their
/// `thread_state` + `retained`. Fires when threads that are BLOCKED or WAITING
/// together hold at least BLOCKED_THREAD_RETAINED_FLOOR bytes AND at least
/// BLOCKED_THREAD_PCT of the heap. A cluster of stuck threads pinning
/// significant memory indicates deadlock, lock contention, or a thread pool
/// that captured large objects in its locals.
struct BlockedThreadConcentration;
impl Rule for BlockedThreadConcentration {
    fn eval(&self, r: &Report) -> Option<TriageSignal> {
        let total = r.leaks.total_shallow;
        if total == 0 {
            return None;
        }
        let (blocked_count, blocked_retained): (usize, u64) = r
            .threads
            .threads
            .iter()
            .filter(|t| {
                let s = t.thread_state.to_ascii_lowercase();
                s.contains("blocked") || s.contains("waiting")
            })
            .fold((0, 0u64), |(c, ret), t| {
                (c + 1, ret.saturating_add(t.retained))
            });
        if blocked_count == 0 || blocked_retained < BLOCKED_THREAD_RETAINED_FLOOR {
            return None;
        }
        let pct = pct_of(blocked_retained, total);
        if pct < BLOCKED_THREAD_PCT {
            return None;
        }
        Some(signal(
            "blocked-thread-concentration",
            TriageSeverity::Warning,
            "Blocked/Waiting Threads Holding Heap",
            format!(
                "{} BLOCKED or WAITING thread{} collectively retain {} ({:.1}% of heap) — \
                 stuck threads are pinning significant memory. Check for deadlocks, \
                 lock contention, or thread-pool threads that captured large objects \
                 in local variables and are now blocked waiting for I/O or a monitor.",
                blocked_count,
                if blocked_count == 1 { "" } else { "s" },
                format_bytes(blocked_retained),
                pct,
            ),
            Some(("threads", "Threads")),
        ))
    }
}

/// Tiny-collection overhead. Reads `collection_attribution.tiny_overhead`.
/// Present only when `--collections` was passed. Fires when the aggregate wrapper
/// overhead of size-{0,1} collections (empty + singleton) exceeds
/// TINY_COLL_OVERHEAD_FLOOR bytes — thousands of single-element or empty
/// wrappers whose overhead is dominated by the container object itself.
struct TinyCollectionOverhead;
impl Rule for TinyCollectionOverhead {
    fn eval(&self, r: &Report) -> Option<TriageSignal> {
        let ca = r.collection_attribution.as_ref()?;
        let total_overhead: u64 = ca.tiny_overhead.iter().map(|t| t.overhead_bytes).sum();
        if total_overhead < TINY_COLL_OVERHEAD_FLOOR {
            return None;
        }
        // Name the top offender if one stands out.
        let top = ca.tiny_overhead.first();
        let offender_clause = match top {
            Some(t) if t.overhead_bytes >= total_overhead / 2 => format!(
                " — top offender: `{}#{}` ({} empty, {} singleton)",
                t.holder_class,
                t.field,
                fmt_count(t.empty_count),
                fmt_count(t.singleton_count),
            ),
            _ => String::new(),
        };
        Some(signal(
            "tiny-collection-overhead",
            TriageSeverity::Info,
            "Tiny-Collection Wrapper Overhead",
            format!(
                "~{} wasted in empty/singleton collection wrappers{}. \
                 Each holds ≤1 element but pays the full ~80 B object-header + \
                 backing-store cost. Replace empty collections with `List.of()` / \
                 `Collections.emptyList()` sentinels and singletons with \
                 `List.of(element)`, or use lazy initialization.",
                format_bytes(total_overhead),
                offender_clause,
            ),
            Some(("collections", "Collections")),
        ))
    }
}

/// Soft-reference cache expansion. Reads `references.soft`. Always-on. Fires
/// when there are many live soft-reference instances (non-null referents) that
/// together retain >= SOFT_CACHE_RETAINED_FLOOR bytes — a soft-reference-based
/// cache that has grown very large without being evicted, often the silent culprit
/// when the JVM appears healthy until memory pressure forces a full GC.
struct SoftRefCacheExpansion;
impl Rule for SoftRefCacheExpansion {
    fn eval(&self, r: &Report) -> Option<TriageSignal> {
        let soft = r.references.soft.as_ref()?;
        let total = r.overview.total_shallow;
        let live_refs = soft
            .reference_instances
            .saturating_sub(soft.null_referent_count);
        if live_refs < SOFT_CACHE_REF_FLOOR {
            return None;
        }
        // Largest class retained only through soft references is the best signal;
        // fall back to the largest class in the referent histogram.
        let largest_retained: u64 = soft.referent_histogram.iter().map(|c| c.retained).sum();
        if largest_retained < SOFT_CACHE_RETAINED_FLOOR {
            return None;
        }
        let top_class = soft
            .referent_histogram
            .first()
            .map(|c| c.pretty_class.as_str())
            .unwrap_or("(unknown)");
        let pct_clause = if total > 0 {
            format!(" ({:.1}% of heap)", pct_of(largest_retained, total))
        } else {
            String::new()
        };
        Some(signal(
            "soft-ref-cache-expansion",
            TriageSeverity::Warning,
            "Soft-Reference Cache Expansion",
            format!(
                "{} live `SoftReference` objects retain {}{} via cached referents — \
                 dominant referent type: `{}`. Soft-reference caches do not evict until \
                 the JVM is near OOM; a large soft-ref heap can mask a memory leak and \
                 trigger long GC pauses. Consider bounding the cache with a size limit \
                 (`LinkedHashMap` LRU, Caffeine/Guava `softValues()` with `maximumSize`), \
                 or switching to explicit LRU eviction.",
                fmt_count(live_refs),
                format_bytes(largest_retained),
                pct_clause,
                top_class,
            ),
            Some(("references", "References")),
        ))
    }
}

/// Unowned collection sink. Reads `biggest_collections.combined` and looks for
/// large collections whose `owner` field is `None` (no `Class#field` was attributed
/// as the holder). An ownerless collection with many elements and significant
/// retained heap is a strong signal of a static-field-backed or root-held cache
/// that the field-attribution pass could not trace — often the root cause of an OOM
/// that `LargeUnboundedCollection` (which flags any big collection) does not
/// specifically call out.
///
/// Complements rather than replaces `LargeUnboundedCollection`: that rule fires on
/// ANY big collection; this rule fires only when the collection has no identified
/// owner, which raises the urgency and changes the remediation advice.
struct UnownedCollectionSink;
impl Rule for UnownedCollectionSink {
    fn eval(&self, r: &Report) -> Option<TriageSignal> {
        let bc = r.biggest_collections.as_ref()?;
        let total = r.leaks.total_shallow;
        // Find the largest ownerless collection that crosses both thresholds.
        let row = bc.combined.iter().filter(|c| c.owner.is_none()).find(|c| {
            c.elements >= UNOWNED_SINK_ELEMENTS
                || c.retained
                    .map(|ret| ret >= UNOWNED_SINK_RETAINED)
                    .unwrap_or(false)
        })?;
        let retained_str = row
            .retained
            .map(|ret| {
                if total > 0 {
                    format!(
                        ", retaining {} ({:.1}% of heap)",
                        format_bytes(ret),
                        pct_of(ret, total)
                    )
                } else {
                    format!(", retaining {}", format_bytes(ret))
                }
            })
            .unwrap_or_default();
        let value_hint = row
            .dominant_value_type
            .as_deref()
            .map(|t| format!(" Values are predominantly `{t}`."))
            .unwrap_or_default();
        Some(signal_cls(
            "unowned-collection-sink",
            TriageSeverity::Warning,
            "Unowned Collection Sink",
            format!(
                "A `{}` holds {} elements{} with no attributed holder field — \
                 likely reachable via a static field, a GC root, or an indirect \
                 reference chain that field attribution did not resolve.{} \
                 Re-run with `--collections` to get field ownership, then trace \
                 the retaining path in the Object Graph Explorer.",
                row.container_class,
                fmt_count(row.elements),
                retained_str,
                value_hint,
            ),
            Some(("biggest-collections", "Biggest Collections")),
            &row.container_class,
        ))
    }
}

/// Stale JDK version. Reads `overview.jvm_version` and `overview.dump_creation`.
/// Fires when the JDK major.minor release can be dated and was released more than
/// STALE_JDK_DAYS before the dump was captured — an outdated JVM may have known
/// GC bugs, memory leaks in the JDK itself, or regressions fixed in later patches.
/// Uses a hardcoded table of JDK GA release dates (updated per the 6-month
/// OpenJDK cadence; LTS every 2 years, feature releases every 6 months).
struct StaleJdk;
impl Rule for StaleJdk {
    fn eval(&self, r: &Report) -> Option<TriageSignal> {
        let ver_str = r.overview.jvm_version.as_deref()?;
        let dump_ms = r.overview.dump_creation?;
        if dump_ms <= 0 {
            return None;
        }
        let dump_days_since_epoch = dump_ms / 86_400_000;

        // Parse the major version number. Handles both "1.8.0_382" (legacy) and
        // "17.0.9+9" (modern) notation.
        let major = parse_jdk_major(ver_str)?;

        // GA release date as days since Unix epoch for each known major version.
        // Source: https://www.java.com/releases/ / OpenJDK release history.
        // Non-LTS releases are included so stale feature-release detection works.
        let release_days: i64 = match major {
            8 => days_since_epoch(2014, 3, 18),
            9 => days_since_epoch(2017, 9, 21),
            10 => days_since_epoch(2018, 3, 20),
            11 => days_since_epoch(2018, 9, 25),
            12 => days_since_epoch(2019, 3, 19),
            13 => days_since_epoch(2019, 9, 17),
            14 => days_since_epoch(2020, 3, 17),
            15 => days_since_epoch(2020, 9, 15),
            16 => days_since_epoch(2021, 3, 16),
            17 => days_since_epoch(2021, 9, 14),
            18 => days_since_epoch(2022, 3, 22),
            19 => days_since_epoch(2022, 9, 20),
            20 => days_since_epoch(2023, 3, 21),
            21 => days_since_epoch(2023, 9, 19),
            22 => days_since_epoch(2024, 3, 19),
            23 => days_since_epoch(2024, 9, 17),
            24 => days_since_epoch(2025, 3, 18),
            25 => days_since_epoch(2025, 9, 16),
            _ => return None, // unknown future version — don't fire
        };

        let age_days = dump_days_since_epoch - release_days;
        if age_days < STALE_JDK_DAYS {
            return None;
        }

        let lts = matches!(major, 8 | 11 | 17 | 21 | 25);
        let eol_note = if !lts && age_days > 180 {
            " This is a non-LTS release past its 6-month support window — no further patches will be issued."
        } else {
            ""
        };

        Some(signal(
            "stale-jdk",
            TriageSeverity::Info,
            "Stale JDK Version",
            format!(
                "JVM version `{}` (JDK {major}) was released ~{} days before this dump was \
                 captured.{eol_note} Update to the latest patch release to rule out known \
                 JDK memory bugs and GC regressions.",
                ver_str, age_days,
            ),
            Some(("system-overview", "System Overview")),
        ))
    }
}

/// Parse the JDK major version number from a version string.
/// Handles "1.8.0_382-b05" (legacy JDK ≤8), "9.0.4", "17.0.9+9", etc.
fn parse_jdk_major(s: &str) -> Option<u32> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    // Legacy "1.N.*" form (JDK ≤8 reported as "1.8.0_...")
    if let Some(rest) = s.strip_prefix("1.") {
        let major_str = rest.split(['.', '_', '-', '+']).next()?;
        return major_str.parse().ok();
    }
    // Modern "N.minor.patch+build" or "N-ea" form
    let first = s.split(['.', '-', '+']).next()?;
    first.parse().ok()
}

/// Days since Unix epoch (1970-01-01) for a given Gregorian calendar date.
/// Uses the proleptic Gregorian calendar (no leap-second correction needed for
/// coarse "days" precision). Matches chrono's NaiveDate::from_ymd arithmetic.
const fn days_since_epoch(year: i32, month: u32, day: u32) -> i64 {
    // Algorithm: compute Julian Day Number, subtract JDN of 1970-01-01.
    // From Fliegel & Van Flandern (1968), valid for all Gregorian dates.
    let y = year as i64;
    let m = month as i64;
    let d = day as i64;
    let jdn = (1461 * (y + 4800 + (m - 14) / 12)) / 4 + (367 * (m - 2 - 12 * ((m - 14) / 12))) / 12
        - (3 * ((y + 4900 + (m - 14) / 12) / 100)) / 4
        + d
        - 32075;
    // JDN of 1970-01-01 = 2440588
    jdn - 2_440_588
}

/// Worker-pool retention. Reads `leaks.suspects` (biggest class groups) and
/// `threads.threads`. Fires when a single class appears as BOTH a multi-instance
/// group suspect (or top class by retained) AND has >= WORKER_POOL_MIN_INSTANCES
/// instances each holding significant heap — i.e. N same-class worker objects
/// together retaining a dominant share while no single one crosses the
/// single-thread-pinning threshold.
///
/// Motivation: in the VSCode/NetBeans dump, 9 `RequestProcessor$Processor` threads
/// each retained 50-100 MB of parser state; individually none crossed the 20%
/// thread-pinning threshold, but collectively they held 77% of the heap. The
/// existing `HeadlineRetainer` fires on the class group, but doesn't explain WHY
/// (each member is a thread-pool worker holding live task state). This rule bridges
/// that gap with a worker-specific diagnosis.
struct WorkerPoolRetention;
impl Rule for WorkerPoolRetention {
    fn eval(&self, r: &Report) -> Option<TriageSignal> {
        let total = r.leaks.total_shallow;
        if total == 0 {
            return None;
        }

        // Find a class-group suspect whose aggregate retained crosses the threshold,
        // but whose per-instance average is well below the single-object threshold —
        // meaning it's many objects each holding a moderate share, not one outlier.
        let suspect = r.leaks.suspects.iter().find(|s| {
            if s.is_single || s.instance_count < WORKER_POOL_MIN_INSTANCES {
                return false;
            }
            if s.retained < WORKER_POOL_RETAINED_FLOOR {
                return false;
            }
            if pct_of(s.retained, total) < WORKER_POOL_RETAINED_PCT {
                return false;
            }
            // Per-instance average must be < 10% of heap (otherwise single-object
            // rules already cover it, and this isn't really a "pool" pattern).
            let per_instance = s.retained / s.instance_count;
            pct_of(per_instance, total) < 10.0
        })?;

        // Check if the class name looks like a thread/worker/task/processor.
        let cls = &suspect.pretty_class;
        let is_worker_class = [
            "Thread",
            "Worker",
            "Processor",
            "Executor",
            "Task",
            "Runner",
            "Handler",
        ]
        .iter()
        .any(|kw| cls.contains(kw));

        let worker_hint = if is_worker_class {
            " These appear to be worker/thread objects — each one is keeping live \
             task state (parser contexts, request data, open transactions) that \
             should be released when the task completes."
        } else {
            " Each instance is holding a significant share; check whether these \
             objects are being pooled, cached, or accumulated without a release path."
        };

        let per_instance = format_bytes(suspect.retained / suspect.instance_count);
        Some(signal_cls(
            "worker-pool-retention",
            TriageSeverity::Warning,
            "Worker-Pool Object Retention",
            format!(
                "{} instances of `{}` together retain {} ({:.1}% of heap), ~{} each —\
                 no single instance dominates, but the pool as a whole is the leak.{worker_hint} \
                 Check whether completed tasks are being recycled or whether task-local \
                 data is being cleared on return to the pool.",
                suspect.instance_count,
                cls,
                format_bytes(suspect.retained),
                pct_of(suspect.retained, total),
                per_instance,
            ),
            Some(("leak-suspects", "Leak Suspects")),
            cls.as_str(),
        ))
    }
}

/// CGLIB / Spring-CGLIB proxy domain-object accumulation.
///
/// Reads `overview.histogram`. Fires when classes whose name contains
/// `$EnhancerByCGLIB$` or `$$EnhancerBySpringCGLIB$$` (Hibernate/Spring AOP
/// proxies of domain objects) have more than [`CGLIB_PROXY_INSTANCE_FLOOR`]
/// total instances and at least [`CGLIB_PROXY_SHALLOW_FLOOR`] aggregate shallow.
/// A handful of CGLIB-proxied beans is normal; hundreds of thousands of them means
/// proxy instances are being created and retained on every request or batch cycle
/// rather than returned to the pool or garbage-collected after use.
struct CglibProxyAccumulation;
impl Rule for CglibProxyAccumulation {
    fn eval(&self, r: &Report) -> Option<TriageSignal> {
        let mut total_instances: u64 = 0;
        let mut total_shallow: u64 = 0;
        let mut top_class = "";
        let mut top_count: u64 = 0;

        for row in &r.overview.histogram {
            if row.pretty_class.contains("$EnhancerByCGLIB$")
                || row.pretty_class.contains("$$EnhancerBySpringCGLIB$$")
            {
                total_instances += row.instances;
                total_shallow += row.shallow;
                if row.instances > top_count {
                    top_count = row.instances;
                    top_class = &row.pretty_class;
                }
            }
        }

        if total_instances < CGLIB_PROXY_INSTANCE_FLOOR || total_shallow < CGLIB_PROXY_SHALLOW_FLOOR
        {
            return None;
        }

        // Trim the generated suffix for a readable name: keep the base class.
        let base = top_class
            .split("$EnhancerByCGLIB$")
            .next()
            .or_else(|| top_class.split("$$EnhancerBySpringCGLIB$$").next())
            .unwrap_or(top_class);

        Some(signal(
            "cglib-proxy-accumulation",
            TriageSeverity::Warning,
            "CGLIB Proxy Instance Accumulation",
            format!(
                "{} CGLIB-enhanced domain-object instances ({}) are live — far more \
                 than the handful expected from a healthy proxy pool. \
                 The most common proxied type is `{}`. \
                 Hibernate lazy-load proxies and Spring AOP advice proxies are normally \
                 short-lived; large counts indicate that proxied objects are being retained \
                 in a cache, thread-local, or collection that is not cleared after use. \
                 Check whether `EntityManager` / `Session` scopes are leaking across \
                 request boundaries, or whether a cache is holding proxied entities \
                 instead of detached plain objects.",
                fmt_count(total_instances),
                format_bytes(total_shallow),
                base,
            ),
            Some(("system-overview", "Class Histogram")),
        ))
    }
}

/// WeakHashMap accumulation.
///
/// Reads `overview.histogram`. Fires when the count of live `WeakHashMap`
/// instances exceeds [`WEAK_HASHMAP_FLOOR`]. WeakHashMaps are typically used as
/// small-scale caches or listener registries where keys are GC'd when no longer
/// strongly referenced, triggering automatic entry removal. A count in the
/// hundreds of thousands means either (a) a new `WeakHashMap` is created per
/// request/object without a shared registry, or (b) keys are being kept strongly
/// alive elsewhere so entries accumulate without ever being expunged.
struct WeakHashMapAccumulation;
impl Rule for WeakHashMapAccumulation {
    fn eval(&self, r: &Report) -> Option<TriageSignal> {
        let count = r
            .overview
            .histogram
            .iter()
            .find(|row| row.pretty_class == "java.util.WeakHashMap")
            .map(|row| row.instances)
            .unwrap_or(0);

        if count < WEAK_HASHMAP_FLOOR {
            return None;
        }

        Some(signal(
            "weak-hashmap-accumulation",
            TriageSeverity::Warning,
            "WeakHashMap Instance Accumulation",
            format!(
                "{} live `java.util.WeakHashMap` instances — far above the handful \
                 expected from a healthy application. WeakHashMaps are intended as \
                 small-scale caches whose entries expire when their keys are GC'd. \
                 A high count usually means one of: (1) a new WeakHashMap is allocated \
                 per request/object rather than shared, causing unbounded creation; \
                 (2) keys are strongly held elsewhere (e.g. in a static field or \
                 another collection) so entries never expunge; or (3) a framework is \
                 using WeakHashMap as a per-class/per-loader registry and class reloads \
                 are accumulating stale entries. \
                 Search for `new WeakHashMap` call sites and verify that map lifecycles \
                 are bounded.",
                fmt_count(count),
            ),
            Some(("system-overview", "Class Histogram")),
        ))
    }
}

/// Returns `true` if `n` is a power of two and `>= min`.
#[inline]
fn is_power_of_two_ge(n: u64, min: u64) -> bool {
    n >= min && n.is_power_of_two()
}

/// Async logging ring buffer full (Log4j2 / Disruptor).
///
/// Reads `overview.histogram`. Fires when a class whose name contains
/// `RingBufferLogEvent` (Log4j2 async appender) has an instance count that is a
/// power of two and at least [`ASYNC_LOG_RINGBUF_FLOOR`]. Log4j2's AsyncAppender
/// (and LMAX Disruptor-based async loggers) pre-allocate a fixed-size ring buffer
/// of event objects (default 256 KiB = 262,144 slots). Finding *exactly* 2^N live
/// instances is a strong signal that the buffer is fully populated — the
/// application is writing logs faster than the async appender can drain them,
/// which causes application threads to block on log calls, wasting heap, and
/// potentially masking the underlying issue.
struct AsyncLogRingBufferFull;
impl Rule for AsyncLogRingBufferFull {
    fn eval(&self, r: &Report) -> Option<TriageSignal> {
        let row = r
            .overview
            .histogram
            .iter()
            .find(|row| row.pretty_class.contains("RingBufferLogEvent"))?;

        let count = row.instances;
        if !is_power_of_two_ge(count, ASYNC_LOG_RINGBUF_FLOOR) {
            return None;
        }

        Some(signal(
            "async-log-ringbuf-full",
            TriageSeverity::Warning,
            "Async Logging Ring Buffer Full",
            format!(
                "Exactly {} `{}` instances are live — a power of two, which is the \
                 signature of a fully-populated Log4j2 / Disruptor async appender \
                 ring buffer. When the ring buffer is full, application threads block \
                 on every log call until a slot is freed. This indicates the async \
                 appender cannot drain as fast as the application produces log events. \
                 Check: (1) whether log volume spiked during an error storm (exceptions \
                 being logged in a tight loop); (2) whether the async appender's \
                 backing I/O (file, socket, SIEM) is slow or blocked; (3) consider \
                 increasing `<AsyncRoot>` discardThreshold or switching to a \
                 synchronous appender if log ordering is not critical.",
                fmt_count(count),
                row.pretty_class,
            ),
            Some(("system-overview", "Class Histogram")),
        ))
    }
}

/// Map-entry dominance (HashMap$Node / ConcurrentHashMap$Node / LinkedHashMap$Entry).
///
/// Reads `overview.histogram` and `overview.total_objects`. Fires when the
/// combined instance count of map-entry types (`HashMap$Node`,
/// `ConcurrentHashMap$Node`, `LinkedHashMap$Entry`) exceeds
/// [`MAP_ENTRY_INSTANCE_FLOOR`] or constitutes more than [`MAP_ENTRY_OBJECT_PCT`]%
/// of all live objects. A massive number of map entries, relative to the total
/// object count, means the application is accumulating key–value pairs that should
/// have been evicted or their containing maps should have been GC'd. The signal is
/// most actionable when the top leaked suspect is a HashMap or a class that holds
/// one: the maps themselves explain only part of the picture because each map's
/// `HashMap$Node[]` table and each `HashMap$Node` chain-node are separate objects.
struct MapEntryDominance;
impl Rule for MapEntryDominance {
    fn eval(&self, r: &Report) -> Option<TriageSignal> {
        let total_objects = r.overview.total_objects;
        if total_objects == 0 {
            return None;
        }

        let mut total_entries: u64 = 0;
        for row in &r.overview.histogram {
            let cls = row.pretty_class.as_str();
            if cls == "java.util.HashMap$Node"
                || cls == "java.util.concurrent.ConcurrentHashMap$Node"
                || cls == "java.util.LinkedHashMap$Entry"
            {
                total_entries += row.instances;
            }
        }

        if total_entries == 0 {
            return None;
        }

        let pct = pct_of(total_entries, total_objects);
        if total_entries < MAP_ENTRY_INSTANCE_FLOOR && pct < MAP_ENTRY_OBJECT_PCT {
            return None;
        }

        Some(signal(
            "map-entry-dominance",
            TriageSeverity::Warning,
            "Map Entry Objects Dominate Live Set",
            format!(
                "{} map-entry objects (`HashMap$Node` / `ConcurrentHashMap$Node` / \
                 `LinkedHashMap$Entry`) account for {:.1}% of all {} live objects. \
                 This means the heap is filled with accumulated key–value pairs — the \
                 maps holding them are not being cleared, evicted, or GC'd. \
                 Common causes: a cache without an eviction policy; thread-locals that \
                 accumulate a map entry per processed item and are never cleared; a \
                 batch job that builds in-memory indexes without releasing them between \
                 iterations. \
                 Identify the owning `HashMap` instances via the Biggest Collections \
                 section and trace them up the dominator tree to find the GC root \
                 preventing collection.",
                fmt_count(total_entries),
                pct,
                fmt_count(total_objects),
            ),
            Some(("biggest-collections", "Biggest Collections")),
        ))
    }
}

/// Hibernate field/setter interceptor accumulation.
///
/// Reads `overview.histogram`. Fires when classes whose name contains
/// `FieldInterceptor`, `SetterInterceptMethodAdaptor`, or `FieldHandler`
/// (Hibernate bytecode-enhancement artifacts — one instance is created per
/// intercepted field on every enhanced entity) have more than
/// [`HIBERNATE_INTERCEPTOR_FLOOR`] total instances AND at least
/// [`HIBERNATE_INTERCEPTOR_SHALLOW_FLOOR`] aggregate shallow bytes.
///
/// A handful of these is expected; millions means Hibernate-enhanced entities
/// are being accumulated in a session, cache, or collection that is not
/// cleared between requests or batch iterations. Unlike CGLIB proxies (which
/// are class-level), each interceptor wraps a single field on a single entity
/// instance — so a high count tracks entity accumulation directly.
struct HibernateInterceptorAccumulation;
impl Rule for HibernateInterceptorAccumulation {
    fn eval(&self, r: &Report) -> Option<TriageSignal> {
        let mut total_instances: u64 = 0;
        let mut total_shallow: u64 = 0;
        let mut top_class = "";
        let mut top_count: u64 = 0;

        for row in &r.overview.histogram {
            let cls = row.pretty_class.as_str();
            if cls.contains("FieldInterceptor")
                || cls.contains("SetterInterceptMethodAdaptor")
                || cls.contains("FieldHandler")
            {
                total_instances += row.instances;
                total_shallow += row.shallow;
                if row.instances > top_count {
                    top_count = row.instances;
                    top_class = cls;
                }
            }
        }

        if total_instances < HIBERNATE_INTERCEPTOR_FLOOR
            || total_shallow < HIBERNATE_INTERCEPTOR_SHALLOW_FLOOR
        {
            return None;
        }

        Some(signal(
            "hibernate-interceptor-accumulation",
            TriageSeverity::Warning,
            "Hibernate Field Interceptor Accumulation",
            format!(
                "{} Hibernate field/setter interceptor instances ({}) are live — \
                 one interceptor is created per field on each enhanced entity, so this \
                 count tracks retained entity instance count directly. The most common \
                 interceptor type is `{}`. \
                 This indicates Hibernate-enhanced entities are not being released after \
                 use. Common causes: an `EntityManager` or `Session` not closed after a \
                 request; a cache holding managed (not detached) entities; a batch loop \
                 that loads entities into a session without periodic `flush()`/`clear()` \
                 calls. Call `session.evict()` or `entityManager.detach()` after processing \
                 each entity in bulk operations, or use `StatelessSession` for read-only \
                 batch access.",
                fmt_count(total_instances),
                format_bytes(total_shallow),
                top_class,
            ),
            Some(("system-overview", "Class Histogram")),
        ))
    }
}

/// Lock-object proliferation (`ReentrantLock$NonfairSync`, etc.).
///
/// Reads `overview.histogram`. Fires when classes whose name contains
/// `ReentrantLock$` or `ReentrantReadWriteLock$` (the inner `Sync` AQS nodes
/// that back `java.util.concurrent.locks.ReentrantLock`) total more than
/// [`LOCK_OBJECT_FLOOR`] instances. A handful is normal; hundreds of thousands
/// means the application is creating a per-entity or per-record lock, which
/// scales linearly with the entity count and leaks if those entities are cached.
/// It is also a contention risk: many threads competing for many fine-grained
/// locks can serialise on the AQS queue nodes even when different logical
/// resources are involved.
struct LockObjectProliferation;
impl Rule for LockObjectProliferation {
    fn eval(&self, r: &Report) -> Option<TriageSignal> {
        let count: u64 = r
            .overview
            .histogram
            .iter()
            .filter(|row| {
                row.pretty_class.contains("ReentrantLock$")
                    || row.pretty_class.contains("ReentrantReadWriteLock$")
            })
            .map(|row| row.instances)
            .sum();

        if count < LOCK_OBJECT_FLOOR {
            return None;
        }

        Some(signal(
            "lock-object-proliferation",
            TriageSeverity::Warning,
            "Fine-Grained Lock Object Proliferation",
            format!(
                "{} `ReentrantLock` / `ReentrantReadWriteLock` sync objects are live. \
                 At this scale the application is creating one lock per entity or record \
                 rather than using a shared striped-lock structure. The lock objects \
                 accumulate whenever their owning entities are cached or accumulated. \
                 Consider replacing per-entity `ReentrantLock` fields with \
                 `Striped<Lock>` (Guava) or a `ConcurrentHashMap`-based compare-and-swap \
                 approach, which shares a fixed pool of lock objects across all keys \
                 regardless of how many entities are cached.",
                fmt_count(count),
            ),
            Some(("system-overview", "Class Histogram")),
        ))
    }
}

/// Performance-monitoring call-graph retention.
///
/// Reads `overview.histogram`. Fires when classes whose simple name contains
/// `CallNode`, `CallStack`, `CallTree`, or `StackFrame` AND whose package
/// contains `perf`, `profil`, `monitor`, `metric`, or `trace` accumulate more
/// than [`PERF_MONITOR_FLOOR`] instances. These are internal nodes of a
/// call-graph or profiling tree built by in-process APM / performance-logging
/// instrumentation. A large live count means the instrumentation is retaining
/// call graphs indefinitely (e.g. accumulating into a static tree that is never
/// pruned) rather than flushing them after each reporting window.
struct PerfMonitoringRetention;
impl Rule for PerfMonitoringRetention {
    fn eval(&self, r: &Report) -> Option<TriageSignal> {
        let mut total_instances: u64 = 0;
        let mut top_class = "";
        let mut top_count: u64 = 0;

        for row in &r.overview.histogram {
            let cls = &row.pretty_class;
            // Simple-name must look like a call-graph node.
            let simple = cls.rsplit('.').next().unwrap_or(cls.as_str());
            let is_call_node = simple.contains("CallNode")
                || simple.contains("CallStack")
                || simple.contains("CallTree")
                || simple.contains("StackFrame");
            if !is_call_node {
                continue;
            }
            // Package must look like perf/profiling/monitoring/tracing infra.
            let pkg = cls.as_str();
            let is_perf_pkg = pkg.contains("perf")
                || pkg.contains("profil")
                || pkg.contains("monitor")
                || pkg.contains("metric")
                || pkg.contains("trace");
            if !is_perf_pkg {
                continue;
            }
            total_instances += row.instances;
            if row.instances > top_count {
                top_count = row.instances;
                top_class = cls.as_str();
            }
        }

        if total_instances < PERF_MONITOR_FLOOR {
            return None;
        }

        Some(signal(
            "perf-monitoring-retention",
            TriageSeverity::Warning,
            "Performance Monitoring Call-Graph Retention",
            format!(
                "{} performance-monitoring call-graph nodes are live (top type: `{}`). \
                 In-process APM or performance-logging instrumentation is accumulating \
                 call-graph nodes without flushing them after each reporting window. \
                 This often happens when a static call-tree root is appended to on every \
                 instrumented method call but never pruned or reset. \
                 Check the instrumentation library's flush/reset API and ensure it is \
                 called on a bounded interval (e.g. after each request or on a periodic \
                 timer), or disable call-graph collection if only flat timing is needed.",
                fmt_count(total_instances),
                top_class,
            ),
            Some(("system-overview", "Class Histogram")),
        ))
    }
}

/// ThreadLocal value class retention.
///
/// Reads `thread_local_analysis` (populated by `--find-duplicates` /
/// `--full-analysis`). Fires when the aggregate retained heap across all
/// `ThreadLocalMap$Entry` values exceeds [`THREADLOCAL_VALUE_RETAINED_FLOOR`].
/// This complements the existing `threadlocal-leak` rule (which counts null-key
/// stale entries) by surfacing LIVE, non-stale ThreadLocal values that are
/// simply large — e.g. request-scoped objects that survive well beyond their
/// expected scope, or parser/formatter state held per-thread in a pool.
///
/// The signal names the top value class by retained heap so the developer can
/// directly grep for the corresponding `ThreadLocal<T>` declaration.
struct ThreadLocalValueRetention;
impl Rule for ThreadLocalValueRetention {
    fn eval(&self, r: &Report) -> Option<TriageSignal> {
        if r.thread_local_analysis.is_empty() {
            return None;
        }
        let total_retained: u64 = r.thread_local_analysis.iter().map(|row| row.retained).sum();
        if total_retained < THREADLOCAL_VALUE_RETAINED_FLOOR {
            return None;
        }

        // Find the value class that retains the most.
        let top = r
            .thread_local_analysis
            .iter()
            .max_by_key(|row| row.retained)?;

        // Stale-entry fraction across all entries.
        let total_entries: u32 = r
            .thread_local_analysis
            .iter()
            .map(|row| row.entry_count)
            .sum();
        let total_stale: u32 = r
            .thread_local_analysis
            .iter()
            .map(|row| row.stale_count)
            .sum();
        let stale_note = if total_entries > 0 && total_stale * 10 >= total_entries {
            // >= 10% stale
            format!(
                " ({:.0}% of entries are stale — keys GC'd but values still held)",
                total_stale as f64 / total_entries as f64 * 100.0
            )
        } else {
            String::new()
        };

        let top_note = if top.entry_count >= THREADLOCAL_VALUE_ENTRY_FLOOR {
            format!(
                " The largest contributor is `{}` ({} entries, {}).",
                top.value_class,
                fmt_count(top.entry_count as u64),
                format_bytes(top.retained),
            )
        } else {
            String::new()
        };

        Some(signal(
            "threadlocal-value-retention",
            TriageSeverity::Warning,
            "ThreadLocal Value Retention",
            format!(
                "{} retained across {} ThreadLocal entries{stale_note}.{top_note} \
                 Large ThreadLocal values mean each thread in the pool holds its own \
                 copy of significant data that outlives the logical request or task. \
                 Ensure values are `remove()`d at task boundaries, or use \
                 request-scoped injection (CDI/Spring `@RequestScope`) instead of \
                 raw ThreadLocals.",
                format_bytes(total_retained),
                fmt_count(total_entries as u64),
            ),
            Some(("thread-local-analysis", "ThreadLocal Analysis")),
        ))
    }
}

/// Humongous primitive-array allocation.
///
/// Reads `collections.top_prim_arrays.top_individual`. Fires when the single
/// largest primitive array is ≥ [`HUMONGOUS_ARRAY_FLOOR`] bytes (default 4 MB).
/// G1GC classifies allocations larger than half a heap region (typically 0.5–4 MB
/// depending on heap size and `-XX:G1HeapRegionSize`) as "humongous". Humongous
/// objects bypass the normal young-generation allocation path and are placed
/// directly into old-gen regions, which G1 then dedicates entirely to that one
/// object. This causes fragmentation — each humongous region is only partially
/// used — and forces more frequent full-GC cycles to reclaim them.
///
/// Common sources: large serialised payloads (`byte[]`), JDBC result-set
/// buffers, in-memory image data, or inflate-then-keep patterns where a
/// decompressed buffer grows to a multiple of its compressed size.
struct HumongousObjectAllocation;
impl Rule for HumongousObjectAllocation {
    fn eval(&self, r: &Report) -> Option<TriageSignal> {
        let top = r.collections.top_prim_arrays.top_individual.first()?;
        if top.shallow < HUMONGOUS_ARRAY_FLOOR {
            return None;
        }
        let owner_note = match &top.owner {
            Some(o) => format!(" held by `{o}`"),
            None => String::new(),
        };
        Some(signal(
            "humongous-object-allocation",
            TriageSeverity::Info,
            "Humongous Primitive Array",
            format!(
                "The largest primitive array is a `{}` of {} ({} elements){owner_note}. \
                 Arrays this size are allocated as G1GC \"humongous\" objects: they \
                 bypass the young generation and are placed directly in dedicated \
                 old-gen regions, causing heap fragmentation and more frequent \
                 full-GC cycles. If this array is transient (e.g. a decode buffer or \
                 a network read buffer), consider pooling it with a `ByteBuffer` pool \
                 or splitting the work into smaller chunks. If it is long-lived, \
                 verify that its size is bounded and expected.",
                top.array_class,
                format_bytes(top.shallow),
                fmt_count(top.length),
            ),
            Some(("collections", "Collections")),
        ))
    }
}

/// Component (class-loader) retention imbalance.
///
/// Reads `top_components`. Fires when the single top component retains
/// ≥ [`COMPONENT_IMBALANCE_TOP_PCT`]% of the heap AND there are at least
/// [`COMPONENT_IMBALANCE_MIN_COMPONENTS`] components — meaning one plugin,
/// module, or application within a multi-app server monopolizes the heap while
/// all others are comparatively small. In OSGi, Jakarta EE, or embedded
/// classloader architectures (Tomcat, OSGi Felix, JBoss modules) this pattern
/// typically indicates that one deployed component has a retention leak while
/// the others are healthy.
struct ComponentRetentionImbalance;
impl Rule for ComponentRetentionImbalance {
    fn eval(&self, r: &Report) -> Option<TriageSignal> {
        let comps = &r.top_components.components;
        if comps.len() < COMPONENT_IMBALANCE_MIN_COMPONENTS {
            return None;
        }
        let top = comps.first()?;
        if top.pct < COMPONENT_IMBALANCE_TOP_PCT {
            return None;
        }
        // Second component for comparison.
        let second = &comps[1];
        Some(signal(
            "component-retention-imbalance",
            TriageSeverity::Warning,
            "Component Retention Imbalance",
            format!(
                "Component `{}` retains {:.1}% of heap ({}) while the next \
                 largest component (`{}`) retains only {:.1}%. In a multi-module \
                 or multi-app deployment (OSGi, EE, Tomcat) this imbalance means \
                 one component dominates the heap. Investigate whether a \
                 class-loader-scoped cache, static field, or event-listener \
                 registration in `{}` is accumulating without a release path.",
                top.loader_label,
                top.pct,
                format_bytes(top.retained),
                second.loader_label,
                second.pct,
                top.loader_label,
            ),
            Some(("top-components", "Top Components")),
        ))
    }
}

/// Exception-object accumulation. Reads `overview.histogram`. Fires when a
/// class whose name ends with `Exception` or `Error` (but not `*ErrorCode`,
/// `*ErrorMessage`, `*ErrorHandler`, etc.) accumulates >= 50 K live instances
/// with aggregate shallow >= 16 MB. Each exception object carries a
/// StackTraceElement[] that can itself retain dozens of String/char[] objects;
/// tight error-retry loops that keep references to thrown exceptions are a
/// reliable OOM vector.
struct ExceptionObjectAccumulation;
impl Rule for ExceptionObjectAccumulation {
    fn eval(&self, r: &Report) -> Option<TriageSignal> {
        // Collect all exception/error rows that meet both floors, pick the worst.
        let rows: Vec<_> = r
            .overview
            .histogram
            .iter()
            .filter(|h| {
                let c = &h.pretty_class;
                // Must end with "Exception" or "Error" but not noise suffixes.
                let is_exc = c.ends_with("Exception") || c.ends_with("Error");
                let is_noise = c.ends_with("ErrorCode")
                    || c.ends_with("ErrorMessage")
                    || c.ends_with("ErrorHandler")
                    || c.ends_with("ErrorType")
                    || c.ends_with("ErrorListener");
                is_exc
                    && !is_noise
                    && h.instances >= EXCEPTION_ACCUM_FLOOR
                    && h.shallow >= EXCEPTION_ACCUM_SHALLOW_FLOOR
            })
            .collect();
        if rows.is_empty() {
            return None;
        }
        // Report the single worst offender (most instances).
        let worst = rows.iter().max_by_key(|h| h.instances)?;
        let total_instances: u64 = rows.iter().map(|h| h.instances).sum();
        let total_shallow: u64 = rows.iter().map(|h| h.shallow).sum();
        let detail = if rows.len() == 1 {
            format!(
                "`{}` has {} live instances (shallow {}). Each exception retains a \
                 StackTraceElement[] plus String/char[] for class/method/file names; \
                 tight error-retry loops that hold references to thrown exceptions \
                 cause rapid heap growth. Look for catch blocks that store exceptions \
                 in collections/queues, or memoized failures, and add explicit \
                 nulling or capacity caps.",
                worst.pretty_class,
                fmt_count(worst.instances),
                format_bytes(worst.shallow),
            )
        } else {
            format!(
                "{} exception/error classes accumulate a combined {} instances ({}) — \
                 largest: `{}` ({} instances). Each exception retains a \
                 StackTraceElement[] plus String/char[] for class/method/file names; \
                 tight error-retry loops that hold references to thrown exceptions \
                 cause rapid heap growth. Look for catch blocks that store exceptions \
                 in collections/queues, or memoized failures, and add explicit \
                 nulling or capacity caps.",
                rows.len(),
                fmt_count(total_instances),
                format_bytes(total_shallow),
                worst.pretty_class,
                fmt_count(worst.instances),
            )
        };
        Some(signal_cls(
            "exception-object-accumulation",
            TriageSeverity::Warning,
            "Exception Object Accumulation",
            detail,
            Some(("system-overview", "System Overview")),
            &worst.pretty_class,
        ))
    }
}

/// Daemon-thread retention. Reads `threads.threads`. Fires when a single daemon
/// thread retains >= 15% of the heap AND >= 64 MB absolute. Daemon threads are
/// meant to be lightweight background workers (GC helpers, timers, I/O pollers);
/// one holding a large retained heap is unusual and indicates an unbounded
/// cache, queue, or circular reference reachable only through that thread's
/// locals or instance fields.
struct DaemonThreadRetention;
impl Rule for DaemonThreadRetention {
    fn eval(&self, r: &Report) -> Option<TriageSignal> {
        let total = r.overview.total_shallow;
        if total == 0 {
            return None;
        }
        let worst = r
            .threads
            .threads
            .iter()
            .filter(|t| t.is_daemon && t.retained >= DAEMON_RETAINED_FLOOR)
            .max_by_key(|t| t.retained)?;
        if pct_of(worst.retained, total) < DAEMON_RETAINED_PCT {
            return None;
        }
        let name = worst.name.as_deref().unwrap_or("<unnamed>");
        let class = worst.class_name.as_deref().unwrap_or("<unknown>");
        Some(signal(
            "daemon-thread-retention",
            TriageSeverity::Warning,
            "Daemon Thread Retains Large Heap Share",
            format!(
                "Daemon thread `{}` ({}) retains {} ({:.1}% of heap). Daemon threads \
                 should be lightweight background workers; one holding this much heap \
                 indicates an unbounded cache, queue, or circular reference reachable \
                 only through that thread's locals or instance fields. Inspect the \
                 thread's stack and local variables with `--thread-locals` to identify \
                 the retaining path.",
                name,
                class,
                format_bytes(worst.retained),
                pct_of(worst.retained, total),
            ),
            Some(("threads", "Threads")),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::model::*;

    /// Minimal all-zero report the rules can be poked at individually.
    fn base_report() -> Report {
        Report {
            schema_version: SCHEMA_VERSION,
            generated: String::new(),
            truncated_input: false,
            redacted_input: false,
            overview: SystemOverview::default(),
            leaks: LeakSuspects::default(),
            top: TopConsumers::default(),
            threads: ThreadOverview::default(),
            top_components: TopComponents::default(),
            alloc_sites: None,
            arrays_by_size: ArraysBySize::default(),
            dominator_analysis: DominatorAnalysis::default(),
            collections: CollectionsAnalysis::default(),
            references: ReferencesAnalysis::default(),
            collection_attribution: None,
            fields_by_size: None,
            biggest_collections: None,
            collection_contents: None,
            leak_indicators: LeakIndicators::default(),
            triage: Vec::new(),
            waste_summary: None,
            top_retainers: Vec::new(),
            queries: Vec::new(),
            analysis_flags: Default::default(),
            obj_graph_flat: None,
            type_ref_graph: vec![],
            thread_local_analysis: Vec::new(),
            framework_analysis: Vec::new(),
            field_stats: None,
        }
    }

    #[test]
    fn off_heap_fires_above_floor_not_below() {
        let mut r = base_report();
        r.leak_indicators.direct_byte_buffer_capacity_sum = 1024;
        assert!(OffHeap.eval(&r).is_none(), "1 KiB must not fire off-heap");

        r.leak_indicators.direct_byte_buffer_capacity_sum = 128 * 1024 * 1024;
        let s = OffHeap.eval(&r).expect("128 MiB must fire off-heap");
        assert_eq!(s.id, "off-heap");
        assert_eq!(s.anchor.as_deref(), Some("off-heap-nio"));
    }

    #[test]
    fn thread_pinning_by_share_and_by_local_count() {
        let mut r = base_report();
        r.leaks.total_shallow = 1000;

        // By retained share (25% >= 20%).
        r.threads.threads = vec![ThreadInfo {
            retained: 250,
            local_root_count: 0,
            name: Some("worker-1".into()),
            ..Default::default()
        }];
        let s = ThreadPinning.eval(&r).expect("25% share must fire");
        assert!(s.detail.contains("worker-1"));

        // Many local roots AND non-trivial share (150 locals, 12% >= 10%).
        r.threads.threads = vec![ThreadInfo {
            retained: 120,
            local_root_count: 150,
            name: Some("pinner".into()),
            ..Default::default()
        }];
        assert!(
            ThreadPinning.eval(&r).is_some(),
            "150 locals at 12% share must fire"
        );

        // Many local roots but trivial share (150 locals, 1% < 10%): the
        // min-share gate keeps normal threads like `main` from firing.
        r.threads.threads = vec![ThreadInfo {
            retained: 10,
            local_root_count: 150,
            name: Some("main".into()),
            ..Default::default()
        }];
        assert!(
            ThreadPinning.eval(&r).is_none(),
            "150 locals at 1% share must not fire"
        );

        // Neither condition met.
        r.threads.threads = vec![ThreadInfo {
            retained: 10,
            local_root_count: 5,
            name: Some("idle".into()),
            ..Default::default()
        }];
        assert!(ThreadPinning.eval(&r).is_none());
    }

    #[test]
    fn gc_waste_names_the_garbage_root_class() {
        let mut r = base_report();
        r.overview.heap_fragmentation_ratio = 0.05;
        assert!(GcWaste.eval(&r).is_none(), "5% must not fire");

        r.overview.heap_fragmentation_ratio = 0.25;
        r.overview.unreachable_shallow = 500;
        r.overview.unreachable_retained = 900;
        r.overview.unreachable_garbage_roots = vec![UnreachableGarbageRoot {
            pretty_class: "com.example.Cache".into(),
            retained: 800,
            objects: 3,
            children: vec![],
        }];
        let s = GcWaste.eval(&r).expect("25% must fire");
        assert!(s.detail.contains("com.example.Cache"));
        assert!(s.detail.contains("25.0%"));
    }

    #[test]
    fn concentration_owner_join_when_single_suspect_matches_biggest() {
        let mut r = base_report();
        r.leaks.total_shallow = 1000;
        r.leaks.suspects = vec![Suspect {
            is_single: true,
            pretty_class: "com.example.Big".into(),
            instance_count: 1,
            retained: 800,
            ..Default::default()
        }];
        r.top.biggest_objects = vec![ObjRow {
            display_class: "com.example.Big".into(),
            retained: 800,
            owner: Some("com.example.Holder#field".into()),
            ..Default::default()
        }];
        let s = Concentration.eval(&r).expect("always fires");
        assert!(s.detail.contains("highly concentrated"));
        assert!(s.detail.contains("held by `com.example.Holder#field`"));
    }

    #[test]
    fn over_capacity_and_constant_arrays_silent_without_collections() {
        // Default CollectionsAnalysis => tracked == 0, empty constant arrays.
        let r = base_report();
        assert!(OverCapacityCollections.eval(&r).is_none());
        assert!(ConstantValueArrays.eval(&r).is_none());
    }

    #[test]
    fn evaluate_triage_preserves_registry_order() {
        // Build a report that fires headline + concentration + gc-waste, and
        // assert they appear in registry order.
        let mut r = base_report();
        r.leaks.total_shallow = 1000;
        r.leaks.suspects = vec![Suspect {
            is_single: true,
            pretty_class: "A".into(),
            instance_count: 1,
            retained: 900,
            ..Default::default()
        }];
        r.overview.heap_fragmentation_ratio = 0.5;
        r.overview.unreachable_shallow = 500;
        let fired = evaluate_triage(&r);
        let ids: Vec<&str> = fired.iter().map(|s| s.id.as_str()).collect();
        let hp = ids.iter().position(|&x| x == "headline-retainer").unwrap();
        let cp = ids.iter().position(|&x| x == "concentration").unwrap();
        let gp = ids.iter().position(|&x| x == "gc-waste").unwrap();
        assert!(hp < cp && cp < gp, "order was {ids:?}");
    }

    #[test]
    fn object_swarm_fires_on_tiny_class_with_huge_count() {
        let mut r = base_report();
        r.overview.total_shallow = 1_000_000;
        r.overview.histogram = vec![HistRow {
            pretty_class: "com.app.Event".into(),
            instances: 15_000_000,
            shallow: 200_000, // avg 13 bytes — well under SWARM_MAX_INSTANCE_BYTES
            retained: 200_000,
            max_instance_shallow: 13,
            incoming_ref_count: 0,
            loader_id: 0,
            loader_label: None,
            root_path: None,
        }];
        let s = ObjectSwarm
            .eval(&r)
            .expect("15M tiny objects at 20% must fire");
        assert!(s.detail.contains("com.app.Event"));

        // Under threshold: only 1M instances.
        r.overview.histogram[0].instances = 1_000_000;
        assert!(ObjectSwarm.eval(&r).is_none());
    }

    #[test]
    fn boxed_primitive_bloat_fires_on_many_long_instances() {
        let mut r = base_report();
        r.overview.total_shallow = 1_000_000;
        r.overview.histogram = vec![HistRow {
            pretty_class: "java.lang.Long".into(),
            instances: 8_000_000,
            shallow: 128_000_000,
            retained: 128_000_000,
            max_instance_shallow: 16,
            incoming_ref_count: 0,
            loader_id: 0,
            loader_label: None,
            root_path: None,
        }];
        let s = BoxedPrimitiveBloat
            .eval(&r)
            .expect("8M Long instances must fire");
        assert!(s.detail.contains("java.lang.Long"));

        // Non-boxed class doesn't trigger.
        r.overview.histogram[0].pretty_class = "com.example.Foo".into();
        assert!(BoxedPrimitiveBloat.eval(&r).is_none());
    }

    #[test]
    fn classloader_explosion_fires_above_threshold() {
        let mut r = base_report();
        r.overview.classloaders_loaded = 2000;
        assert!(ClassloaderExplosion.eval(&r).is_some());
        r.overview.classloaders_loaded = 50;
        assert!(ClassloaderExplosion.eval(&r).is_none());
    }

    #[test]
    fn thread_swarm_fires_on_high_count() {
        let mut r = base_report();
        r.leaks.total_shallow = 1_000_000;
        // By count >= 1000.
        r.threads.threads = (0..1500)
            .map(|i| ThreadInfo {
                retained: 100,
                name: Some(format!("worker-{i}")),
                ..Default::default()
            })
            .collect();
        assert!(ThreadSwarm.eval(&r).is_some(), "1500 threads must fire");

        // Below count floor: silent even with high aggregate share.
        r.threads.threads = r.threads.threads[0..10].to_vec();
        assert!(ThreadSwarm.eval(&r).is_none());
    }

    #[test]
    fn duplicate_strings_fires_and_silent_without_data() {
        let mut r = base_report();
        // No --find-duplicates data: silent.
        assert!(DuplicateStrings.eval(&r).is_none());

        r.overview.duplicate_strings = Some(crate::pass2::DupStrings {
            approx_wasted_bytes: 32 * 1024 * 1024,
            duplicated_values: 50_000,
            total_string_instances: 200_000,
            ..Default::default()
        });
        let s = DuplicateStrings.eval(&r).expect("32 MiB must fire");
        assert_eq!(s.id, "duplicate-strings");

        // Below floor and below pct: silent.
        r.overview
            .duplicate_strings
            .as_mut()
            .unwrap()
            .approx_wasted_bytes = 1024;
        assert!(DuplicateStrings.eval(&r).is_none());
    }

    #[test]
    fn char_array_slack_fires_and_silent_without_data() {
        let mut r = base_report();
        assert!(CharArraySlack.eval(&r).is_none());

        r.overview.duplicate_strings = Some(crate::pass2::DupStrings {
            char_array_waste: Some(crate::pass2::CharArrayWaste {
                arrays_examined: 100_000,
                wasteful_arrays: 50_000,
                total_wasted_bytes: 32 * 1024 * 1024,
                top: Vec::new(),
            }),
            ..Default::default()
        });
        let s = CharArraySlack.eval(&r).expect("32 MiB slack must fire");
        assert_eq!(s.id, "char-array-slack");

        // Too few wasteful arrays: silent.
        r.overview
            .duplicate_strings
            .as_mut()
            .unwrap()
            .char_array_waste
            .as_mut()
            .unwrap()
            .wasteful_arrays = 10;
        assert!(CharArraySlack.eval(&r).is_none());
    }

    #[test]
    fn large_unbounded_collection_fires_on_element_count() {
        let mut r = base_report();
        r.leaks.total_shallow = 10_000_000;
        // No biggest_collections: silent.
        assert!(LargeUnboundedCollection.eval(&r).is_none());

        r.biggest_collections = Some(BiggestCollections {
            combined: vec![BiggestCollectionRow {
                kind: "Map".into(),
                container_class: "java.util.HashMap".into(),
                elements: 2_000_000,
                retained: Some(4_000_000),
                owner: None,
                dominant_value_type: None,
                value_type_breakdown: Vec::new(),
                obj_index_1based: None,
            }],
            by_kind: Vec::new(),
            truncated: false,
        });
        let s = LargeUnboundedCollection
            .eval(&r)
            .expect("2M elements must fire");
        assert!(s.detail.contains("java.util.HashMap"));

        // Below 1M elements and below retained share: silent.
        r.biggest_collections.as_mut().unwrap().combined[0].elements = 100;
        r.biggest_collections.as_mut().unwrap().combined[0].retained = Some(100);
        assert!(LargeUnboundedCollection.eval(&r).is_none());
    }

    // ── Batch-3 tests ────────────────────────────────────────────────────────

    fn hist_row(class: &str, instances: u64, shallow: u64) -> HistRow {
        HistRow {
            pretty_class: class.into(),
            instances,
            shallow,
            retained: shallow,
            max_instance_shallow: shallow.checked_div(instances).unwrap_or(0),
            incoming_ref_count: 0,
            loader_id: 0,
            loader_label: None,
            root_path: None,
        }
    }

    #[test]
    fn finalizer_fires_on_high_count() {
        let mut r = base_report();
        r.overview.histogram = vec![hist_row("java.lang.ref.Finalizer", 20_000, 640_000)];
        assert!(FinalizerQueueBacklog.eval(&r).is_some());
        r.overview.histogram[0].instances = 100;
        assert!(FinalizerQueueBacklog.eval(&r).is_none());
        // Not present at all: silent.
        r.overview.histogram = vec![];
        assert!(FinalizerQueueBacklog.eval(&r).is_none());
    }

    #[test]
    fn metaspace_pressure_fires_on_high_class_count() {
        let mut r = base_report();
        r.overview.classes_loaded = 60_000;
        assert!(MetaspacePressure.eval(&r).is_some());
        r.overview.classes_loaded = 5_000;
        assert!(MetaspacePressure.eval(&r).is_none());
    }

    #[test]
    fn cached_reflection_fires_on_method_count() {
        let mut r = base_report();
        r.overview.histogram = vec![
            hist_row("java.lang.reflect.Method", 400_000, 25_600_000),
            hist_row("java.lang.reflect.Field", 200_000, 9_600_000),
        ];
        let s = CachedReflectionMetadata
            .eval(&r)
            .expect("600k reflect objects must fire");
        assert!(s.detail.contains("600,000"));
        r.overview.histogram[0].instances = 100;
        r.overview.histogram[1].instances = 100;
        assert!(CachedReflectionMetadata.eval(&r).is_none());
    }

    #[test]
    fn jni_global_ref_fires_on_count_and_share() {
        let mut r = base_report();
        r.overview.total_shallow = 1_000_000;
        r.overview.gc_roots_by_type = vec![crate::report::model::GcRootTypeRow {
            root_type: "JNI Global".into(),
            count: 8_000,
        }];
        r.overview.gc_roots_retained_by_type = vec![crate::report::model::GcRootRetainedRow {
            root_type: "JNI Global".into(),
            count: 8_000,
            retained: 100_000, // 10%
            top_classes: Vec::new(),
        }];
        assert!(JniGlobalRefLeak.eval(&r).is_some());

        // Count too low.
        r.overview.gc_roots_by_type[0].count = 10;
        assert!(JniGlobalRefLeak.eval(&r).is_none());

        // Count high but share too low.
        r.overview.gc_roots_by_type[0].count = 8_000;
        r.overview.gc_roots_retained_by_type[0].retained = 10; // 0.001%
        assert!(JniGlobalRefLeak.eval(&r).is_none());
    }

    #[test]
    fn heap_composition_skew_fires_on_dominant_kind() {
        let mut r = base_report();
        r.overview.total_shallow = 1_000_000;
        r.overview.heap_composition.by_kind = vec![
            crate::report::model::KindStat {
                kind: "Primitive Arrays".into(),
                objects: 10_000,
                shallow_heap: 750_000,
            },
            crate::report::model::KindStat {
                kind: "Instances".into(),
                objects: 50_000,
                shallow_heap: 250_000,
            },
        ];
        let s = HeapCompositionSkew
            .eval(&r)
            .expect("75% primitive arrays must fire");
        assert!(s.detail.contains("Primitive Arrays"));

        // Not dominant enough.
        r.overview.heap_composition.by_kind[0].shallow_heap = 500_000; // 50%
        assert!(HeapCompositionSkew.eval(&r).is_none());
    }

    #[test]
    fn static_field_anchor_fires_when_sticky_class_dominates() {
        let mut r = base_report();
        r.leaks.total_shallow = 1_000_000;
        r.leaks.suspects = vec![Suspect {
            pretty_class: "com.example.AppConfig".into(),
            is_single: true,
            instance_count: 1,
            retained: 400_000,
            root_type_label: "Sticky Class".into(),
            ..Default::default()
        }];
        let s = StaticFieldAnchor
            .eval(&r)
            .expect("40% sticky class must fire");
        assert!(s.detail.contains("AppConfig"));

        // Different root type: silent.
        r.leaks.suspects[0].root_type_label = "Thread".into();
        assert!(StaticFieldAnchor.eval(&r).is_none());

        // Sticky class but low share.
        r.leaks.suspects[0].root_type_label = "Sticky Class".into();
        r.leaks.suspects[0].retained = 100; // 0.01%
        assert!(StaticFieldAnchor.eval(&r).is_none());
    }

    #[test]
    fn session_scope_leak_fires_on_name_pattern() {
        let mut r = base_report();
        r.overview.histogram = vec![hist_row("com.example.UserSession", 200_000, 3_200_000)];
        let s = SessionScopeLeak
            .eval(&r)
            .expect("200k UserSession must fire");
        assert!(s.detail.contains("UserSession"));
        r.overview.histogram[0].instances = 10;
        assert!(SessionScopeLeak.eval(&r).is_none());
    }

    #[test]
    fn connection_leak_fires_on_name_pattern() {
        let mut r = base_report();
        r.overview.histogram = vec![hist_row("com.mysql.jdbc.ConnectionImpl", 5_000, 800_000)];
        let s = ConnectionLeak
            .eval(&r)
            .expect("5000 ConnectionImpl must fire");
        assert!(s.detail.contains("ConnectionImpl"));
        r.overview.histogram[0].instances = 5;
        assert!(ConnectionLeak.eval(&r).is_none());
    }

    #[test]
    fn event_listener_fires_on_name_pattern() {
        let mut r = base_report();
        r.overview.histogram = vec![hist_row("com.example.MessageListener", 150_000, 2_400_000)];
        assert!(EventListenerAccumulation.eval(&r).is_some());
        r.overview.histogram[0].instances = 1_000;
        assert!(EventListenerAccumulation.eval(&r).is_none());
    }

    #[test]
    fn parser_output_fires_on_package_pattern() {
        let mut r = base_report();
        r.overview.histogram = vec![hist_row(
            "com.fasterxml.jackson.databind.node.ObjectNode",
            200_000,
            6_400_000,
        )];
        assert!(ParserOutputAccumulation.eval(&r).is_some());
        r.overview.histogram[0].instances = 10;
        assert!(ParserOutputAccumulation.eval(&r).is_none());
        // Non-parser package: silent.
        r.overview.histogram[0].instances = 500_000;
        r.overview.histogram[0].pretty_class = "com.example.Node".into();
        assert!(ParserOutputAccumulation.eval(&r).is_none());
    }

    #[test]
    fn interned_string_bloat_requires_both_conditions() {
        let mut r = base_report();
        r.overview.histogram = vec![hist_row("java.lang.String", 3_000_000, 96_000_000)];
        r.overview.gc_roots_by_type = vec![crate::report::model::GcRootTypeRow {
            root_type: "JNI Global".into(),
            count: 5_000,
        }];
        assert!(InternedStringBloat.eval(&r).is_some());

        // Too few strings.
        r.overview.histogram[0].instances = 100;
        assert!(InternedStringBloat.eval(&r).is_none());

        // Enough strings but too few JNI globals.
        r.overview.histogram[0].instances = 3_000_000;
        r.overview.gc_roots_by_type[0].count = 5;
        assert!(InternedStringBloat.eval(&r).is_none());
    }

    #[test]
    fn sparse_object_arrays_fires_on_low_fill() {
        let mut r = base_report();
        r.leaks.total_shallow = 1_000_000;
        // No --collections data: silent.
        assert!(SparseObjectArrays.eval(&r).is_none());

        r.collections.array_fill_ratio = crate::report::model::ArrayFillRatio {
            tracked: 50_000,
            buckets: vec![crate::report::model::FillRatioBucket {
                lower_ratio_bp: 0,
                upper_ratio_bp: 2_000, // ≤20%
                objects: 30_000,
                shallow: 600_000,
                wasted: 100_000, // 10% of heap
            }],
        };
        assert!(SparseObjectArrays.eval(&r).is_some());

        // Wasted share too low.
        r.collections.array_fill_ratio.buckets[0].wasted = 10;
        assert!(SparseObjectArrays.eval(&r).is_none());
    }

    #[test]
    fn big_drop_concentration_fires_on_large_drop() {
        let mut r = base_report();
        r.overview.total_shallow = 200 * 1024 * 1024;
        r.dominator_analysis.big_drops.rows = vec![crate::report::model::BigDropRow {
            obj_index_1based: 1,
            display_class: "com.example.Cache".into(),
            retained: 150 * 1024 * 1024,
            child_count: 5,
            largest_child_retained: 10 * 1024 * 1024,
            largest_child_class: "java.util.HashMap".into(),
            drop_bytes: 140 * 1024 * 1024, // 70% — fires
        }];
        let s = BigDropConcentration.eval(&r).expect("large drop must fire");
        assert!(s.detail.contains("Cache"));

        // Drop too small relative to heap.
        r.overview.total_shallow = 10_000 * 1024 * 1024;
        assert!(BigDropConcentration.eval(&r).is_none());
    }

    #[test]
    fn big_drop_concentration_requires_floor() {
        let mut r = base_report();
        r.overview.total_shallow = 200 * 1024 * 1024;
        // Drop is only 40 MiB (below 64 MiB floor) even though share is 20%.
        r.dominator_analysis.big_drops.rows = vec![crate::report::model::BigDropRow {
            obj_index_1based: 1,
            display_class: "com.example.Foo".into(),
            retained: 50 * 1024 * 1024,
            child_count: 1,
            largest_child_retained: 10 * 1024 * 1024,
            largest_child_class: "java.util.ArrayList".into(),
            drop_bytes: 40 * 1024 * 1024,
        }];
        assert!(BigDropConcentration.eval(&r).is_none());
    }

    #[test]
    fn fixed_per_object_overhead_fires_on_many_small_objects() {
        let mut r = base_report();
        // 5M objects × 16 bytes header = 80 MB; total shallow 200 MB → 40%
        r.overview.total_objects = 5_000_000;
        r.overview.total_shallow = 200 * 1024 * 1024;
        r.overview.identifier_size_bits = 64;
        r.overview.compressed_oops = Some(false);
        let s = FixedPerObjectOverhead
            .eval(&r)
            .expect("40% header overhead must fire");
        assert!(s.detail.contains("5,000,000"));

        // Few objects → overhead low.
        r.overview.total_objects = 10;
        assert!(FixedPerObjectOverhead.eval(&r).is_none());
    }

    #[test]
    fn hash_collision_hotspot_fires_on_dense_maps() {
        let mut r = base_report();
        r.collections.map_collision_ratio = crate::report::model::MapCollisionRatio {
            tracked: 500,
            total: 0,
            buckets: vec![crate::report::model::FillRatioBucket {
                lower_ratio_bp: 9_000,
                upper_ratio_bp: 10_000,
                objects: 400,
                shallow: 0,
                wasted: 0,
            }],
        };
        assert!(HashCollisionHotspot.eval(&r).is_some());

        // Too few tracked maps.
        r.collections.map_collision_ratio.tracked = 5;
        assert!(HashCollisionHotspot.eval(&r).is_none());
    }

    #[test]
    fn empty_collection_cemetery_fires_on_high_empty_share() {
        let mut r = base_report();
        r.collections.collections_by_size = crate::report::model::CollectionsBySize {
            tracked: 1_000,
            empty_count: 800, // 80% — fires
            buckets: vec![],
        };
        assert!(EmptyCollectionCemetery.eval(&r).is_some());

        // Below threshold.
        r.collections.collections_by_size.empty_count = 50;
        assert!(EmptyCollectionCemetery.eval(&r).is_none());
    }

    #[test]
    fn empty_collection_cemetery_fires_on_absolute_count() {
        let mut r = base_report();
        r.collections.collections_by_size = crate::report::model::CollectionsBySize {
            tracked: 2_000_000,
            empty_count: 600_000, // only 30% but > 500k floor
            buckets: vec![],
        };
        assert!(EmptyCollectionCemetery.eval(&r).is_some());
    }

    #[test]
    fn oversized_prim_array_fires_on_huge_array() {
        let mut r = base_report();
        r.overview.total_shallow = 200 * 1024 * 1024;
        r.collections.top_prim_arrays.top_individual = vec![crate::report::model::TopArrayRow {
            array_class: "byte[]".into(),
            length: 100_000_000,
            shallow: 100 * 1024 * 1024, // 50% — fires
            obj_index_1based: 1,
            owner: None,
            non_null: None,
        }];
        let s = OversizedPrimArray.eval(&r).expect("huge array must fire");
        assert!(s.detail.contains("byte[]"));

        // Too small.
        r.collections.top_prim_arrays.top_individual[0].shallow = 1024;
        assert!(OversizedPrimArray.eval(&r).is_none());
    }

    #[test]
    fn duplicate_prim_arrays_fires_on_wasted_bytes() {
        let mut r = base_report();
        r.overview.total_shallow = 200 * 1024 * 1024;
        r.overview.duplicate_prim_arrays = Some(crate::pass2::DupPrimArrays {
            total_wasted_bytes: 20 * 1024 * 1024, // 10% — fires
            rows: vec![],
            top_array_holders: vec![],
        });
        let s = DuplicatePrimArrays
            .eval(&r)
            .expect("large dup-prim waste must fire");
        assert!(s.detail.contains("20.0 MB"));

        // Below floor.
        r.overview
            .duplicate_prim_arrays
            .as_mut()
            .unwrap()
            .total_wasted_bytes = 1024;
        assert!(DuplicatePrimArrays.eval(&r).is_none());
    }

    #[test]
    fn deep_retention_chain_fires_above_threshold() {
        let mut r = base_report();
        r.dominator_analysis.longest_chain_depth = 5_000;
        assert!(
            DeepRetentionChain.eval(&r).is_none(),
            "5k depth must not fire"
        );
        r.dominator_analysis.longest_chain_depth = 15_000;
        let s = DeepRetentionChain.eval(&r).expect("15k depth must fire");
        assert_eq!(s.id, "deep-retention-chain");
        assert!(s.detail.contains("15,000"));
    }

    #[test]
    fn framework_leak_fires_on_dominant_framework() {
        let mut r = base_report();
        r.overview.total_shallow = 1_000_000_000; // 1 GB
        r.framework_analysis = vec![crate::report::model::FrameworkAnalysis {
            framework: "Spring".into(),
            instance_count: 12,
            total_retained: 200_000_000, // 20% of heap
        }];
        let s = FrameworkLeak.eval(&r).expect("20% Spring must fire");
        assert_eq!(s.id, "framework-leak");
        assert!(s.detail.contains("Spring"));

        // Below retained floor.
        r.framework_analysis[0].total_retained = 10_000_000;
        assert!(FrameworkLeak.eval(&r).is_none());
    }

    #[test]
    fn blocked_thread_concentration_fires_on_stuck_threads() {
        let mut r = base_report();
        r.leaks.total_shallow = 1_000_000_000;
        r.threads.threads = vec![
            ThreadInfo {
                thread_state: "[alive, waiting]".into(),
                retained: 150_000_000,
                name: Some("pool-1".into()),
                ..Default::default()
            },
            ThreadInfo {
                thread_state: "[alive, blocked]".into(),
                retained: 100_000_000,
                name: Some("pool-2".into()),
                ..Default::default()
            },
            ThreadInfo {
                thread_state: "[alive, runnable]".into(),
                retained: 10_000_000,
                ..Default::default()
            },
        ];
        let s = BlockedThreadConcentration
            .eval(&r)
            .expect("250 MB blocked must fire");
        assert_eq!(s.id, "blocked-thread-concentration");
        assert!(s.detail.contains("2 BLOCKED"));

        // Only runnable threads → no fire.
        r.threads.threads = vec![ThreadInfo {
            thread_state: "[alive, runnable]".into(),
            retained: 500_000_000,
            ..Default::default()
        }];
        assert!(BlockedThreadConcentration.eval(&r).is_none());
    }

    #[test]
    fn tiny_collection_overhead_fires_above_floor() {
        let mut r = base_report();
        r.collection_attribution = Some(CollectionAttribution {
            most_overall: vec![],
            biggest_single: vec![],
            tiny_overhead: vec![TinyCollectionRow {
                holder_class: "com.app.Node".into(),
                field: "children".into(),
                container_kind: "list".into(),
                empty_count: 500_000,
                singleton_count: 100_000,
                overhead_bytes: 48_000_000, // 48 MB > TINY_COLL_OVERHEAD_FLOOR
            }],
            truncated: false,
        });
        let s = TinyCollectionOverhead
            .eval(&r)
            .expect("48 MB tiny overhead must fire");
        assert_eq!(s.id, "tiny-collection-overhead");
        assert!(s.detail.contains("com.app.Node"));

        // Below floor.
        r.collection_attribution.as_mut().unwrap().tiny_overhead[0].overhead_bytes = 100_000;
        assert!(TinyCollectionOverhead.eval(&r).is_none());
    }

    #[test]
    fn soft_ref_cache_expansion_fires_on_large_live_refs() {
        use crate::report::model::RefStatClassRow;
        let mut r = base_report();
        r.overview.total_shallow = 2_000_000_000;
        r.references.soft = Some(ReferenceStats {
            kind: "Soft".into(),
            reference_instances: 50_000,
            null_referent_count: 100, // almost all live
            referent_histogram: vec![RefStatClassRow {
                pretty_class: "com.example.CachedEntry".into(),
                objects: 49_900,
                shallow: 500_000_000,
                retained: 500_000_000, // 25% of heap
            }],
            only_weakly_retained: vec![],
        });
        let s = SoftRefCacheExpansion
            .eval(&r)
            .expect("500 MB soft-ref cache must fire");
        assert_eq!(s.id, "soft-ref-cache-expansion");
        assert!(s.detail.contains("com.example.CachedEntry"));

        // Too few live refs.
        r.references.soft.as_mut().unwrap().reference_instances = 500;
        r.references.soft.as_mut().unwrap().null_referent_count = 0;
        assert!(SoftRefCacheExpansion.eval(&r).is_none());
    }

    #[test]
    fn unowned_collection_sink_fires_on_large_ownerless_collection() {
        let mut r = base_report();
        r.leaks.total_shallow = 1_000_000_000;
        r.biggest_collections = Some(BiggestCollections {
            combined: vec![
                // Owned collection — must not fire.
                BiggestCollectionRow {
                    kind: "map".into(),
                    container_class: "java.util.HashMap".into(),
                    elements: 500_000,
                    retained: Some(200_000_000),
                    owner: Some("com.app.Cache#store".into()),
                    ..Default::default()
                },
                // Large ownerless collection — should fire.
                BiggestCollectionRow {
                    kind: "map".into(),
                    container_class: "java.util.LinkedHashMap".into(),
                    elements: 800_000,
                    retained: Some(150_000_000),
                    owner: None,
                    dominant_value_type: Some("com.app.Session".into()),
                    ..Default::default()
                },
            ],
            by_kind: vec![],
            truncated: false,
        });
        let s = UnownedCollectionSink
            .eval(&r)
            .expect("large ownerless collection must fire");
        assert_eq!(s.id, "unowned-collection-sink");
        assert!(s.detail.contains("java.util.LinkedHashMap"));
        assert!(s.detail.contains("com.app.Session"));

        // All collections have an owner → silent.
        r.biggest_collections.as_mut().unwrap().combined[1].owner = Some("com.app.Foo#bar".into());
        assert!(UnownedCollectionSink.eval(&r).is_none());

        // Ownerless but below both thresholds → silent.
        r.biggest_collections.as_mut().unwrap().combined[1].owner = None;
        r.biggest_collections.as_mut().unwrap().combined[1].elements = 100;
        r.biggest_collections.as_mut().unwrap().combined[1].retained = Some(1024);
        assert!(UnownedCollectionSink.eval(&r).is_none());
    }

    #[test]
    fn parse_jdk_major_handles_legacy_and_modern() {
        assert_eq!(parse_jdk_major("1.8.0_382-b05"), Some(8));
        assert_eq!(parse_jdk_major("1.7.0_80"), Some(7));
        assert_eq!(parse_jdk_major("11.0.20+8"), Some(11));
        assert_eq!(parse_jdk_major("17.0.9+9"), Some(17));
        assert_eq!(parse_jdk_major("21"), Some(21));
        assert_eq!(parse_jdk_major("21-ea"), Some(21));
        assert_eq!(parse_jdk_major(""), None);
    }

    #[test]
    fn days_since_epoch_known_values() {
        // Cross-checked against Python datetime.
        assert_eq!(days_since_epoch(1970, 1, 1), 0);
        assert_eq!(days_since_epoch(2014, 3, 18), 16147); // JDK 8 GA
        assert_eq!(days_since_epoch(2021, 9, 14), 18884); // JDK 17 GA
        assert_eq!(days_since_epoch(2023, 9, 19), 19619); // JDK 21 GA
    }

    #[test]
    fn stale_jdk_fires_on_old_version_at_dump_time() {
        let mut r = base_report();
        // Dump taken 2024-08-01 = days_since_epoch(2024,8,1) * 86400000 ms
        let dump_day = days_since_epoch(2024, 8, 1); // 19936
        r.overview.dump_creation = Some(dump_day * 86_400_000);

        // JDK 17 GA was 2021-09-14 = day 18884 → age = 19936 - 18884 = 1052 days → fires
        r.overview.jvm_version = Some("17.0.9+9".into());
        let s = StaleJdk.eval(&r).expect("JDK 17 (1052 days old) must fire");
        assert_eq!(s.id, "stale-jdk");
        assert!(s.detail.contains("JDK 17"));

        // JDK 24 GA was 2025-03-18 = day 20165 → age = 19936 - 20165 = -229 → must not fire
        r.overview.jvm_version = Some("24.0.1+9".into());
        assert!(
            StaleJdk.eval(&r).is_none(),
            "JDK 24 not yet released at dump time must not fire"
        );

        // Legacy JDK 8 ("1.8.0_382") GA was 2014-03-18 = day 16147 → very old → fires
        r.overview.jvm_version = Some("1.8.0_382-b05".into());
        let s8 = StaleJdk.eval(&r).expect("JDK 8 must fire");
        assert!(s8.detail.contains("JDK 8"));

        // No dump timestamp → silent.
        r.overview.dump_creation = None;
        assert!(StaleJdk.eval(&r).is_none());
    }

    #[test]
    fn worker_pool_retention_fires_on_multi_instance_group() {
        let mut r = base_report();
        r.leaks.total_shallow = 1_000_000_000; // 1 GB

        // 9 worker objects each holding ~88 MB = 792 MB aggregate = 79.2%
        // Per-instance = 88 MB = 8.8% (below 10% single-object threshold)
        r.leaks.suspects = vec![Suspect {
            is_single: false,
            pretty_class: "org.openide.util.RequestProcessor$Processor".into(),
            instance_count: 9,
            retained: 792_000_000,
            ..Default::default()
        }];
        let s = WorkerPoolRetention
            .eval(&r)
            .expect("9 workers at 79% must fire");
        assert_eq!(s.id, "worker-pool-retention");
        assert!(s.detail.contains("RequestProcessor"));
        // Worker-class hint should appear because name contains "Processor"
        assert!(s.detail.contains("worker/thread"));

        // Single-instance suspects don't fire this rule.
        r.leaks.suspects[0].is_single = true;
        r.leaks.suspects[0].instance_count = 1;
        assert!(WorkerPoolRetention.eval(&r).is_none());

        // Group with per-instance average >= 10% of heap → single-object rules cover it.
        r.leaks.suspects[0].is_single = false;
        r.leaks.suspects[0].instance_count = 3;
        r.leaks.suspects[0].retained = 600_000_000; // 200 MB each = 20% each
        assert!(WorkerPoolRetention.eval(&r).is_none());

        // Group below the floor.
        r.leaks.suspects[0].instance_count = 9;
        r.leaks.suspects[0].retained = 10_000_000; // tiny
        assert!(WorkerPoolRetention.eval(&r).is_none());
    }

    #[test]
    fn cglib_proxy_accumulation_fires_on_many_enhanced_instances() {
        let mut r = base_report();
        r.overview.histogram = vec![
            hist_row(
                "com.example.TimeAccountType$EnhancerByCGLIB$5b6cad54",
                200_000,
                60_000_000,
            ),
            hist_row(
                "com.example.Employee$EnhancerByCGLIB$1a2b3c4d",
                80_000,
                24_000_000,
            ),
            hist_row("java.lang.String", 1_000_000, 32_000_000),
        ];
        let s = CglibProxyAccumulation
            .eval(&r)
            .expect("280k CGLIB proxy instances must fire");
        assert_eq!(s.id, "cglib-proxy-accumulation");
        // Base class name extracted from top entry
        assert!(s.detail.contains("TimeAccountType"));

        // Below instance floor
        r.overview.histogram[0].instances = 1_000;
        r.overview.histogram[1].instances = 1_000;
        assert!(CglibProxyAccumulation.eval(&r).is_none());

        // Above instance floor but below shallow floor
        r.overview.histogram[0].instances = 60_000;
        r.overview.histogram[0].shallow = 100_000; // tiny
        r.overview.histogram[1].instances = 5_000;
        r.overview.histogram[1].shallow = 100_000;
        assert!(CglibProxyAccumulation.eval(&r).is_none());

        // No CGLIB classes at all: silent
        r.overview.histogram = vec![hist_row("java.lang.String", 1_000_000, 32_000_000)];
        assert!(CglibProxyAccumulation.eval(&r).is_none());
    }

    #[test]
    fn weak_hashmap_accumulation_fires_above_floor() {
        let mut r = base_report();
        r.overview.histogram = vec![hist_row("java.util.WeakHashMap", 200_000, 10_000_000)];
        let s = WeakHashMapAccumulation
            .eval(&r)
            .expect("200k WeakHashMap instances must fire");
        assert_eq!(s.id, "weak-hashmap-accumulation");
        assert!(s.detail.contains("200,000"));

        // Below floor
        r.overview.histogram[0].instances = 50;
        assert!(WeakHashMapAccumulation.eval(&r).is_none());

        // Not present
        r.overview.histogram = vec![];
        assert!(WeakHashMapAccumulation.eval(&r).is_none());
    }

    #[test]
    fn async_log_ringbuf_full_fires_on_power_of_two() {
        let mut r = base_report();
        // 65536 = 2^16 — exactly a power of two, >= 512 → fires
        r.overview.histogram = vec![hist_row(
            "org.apache.logging.log4j.core.async.RingBufferLogEvent",
            65_536,
            6_000_000,
        )];
        let s = AsyncLogRingBufferFull
            .eval(&r)
            .expect("65536 RingBufferLogEvent must fire");
        assert_eq!(s.id, "async-log-ringbuf-full");
        assert!(s.detail.contains("65,536"));

        // Non-power-of-two count: silent
        r.overview.histogram[0].instances = 65_000;
        assert!(AsyncLogRingBufferFull.eval(&r).is_none());

        // Below minimum floor (256 = power of two but < 512): silent
        r.overview.histogram[0].instances = 256;
        assert!(AsyncLogRingBufferFull.eval(&r).is_none());

        // Not present at all: silent
        r.overview.histogram = vec![hist_row("java.lang.String", 1_000, 16_000)];
        assert!(AsyncLogRingBufferFull.eval(&r).is_none());
    }

    #[test]
    fn map_entry_dominance_fires_on_count_floor() {
        let mut r = base_report();
        r.overview.total_objects = 200_000_000;
        r.overview.histogram = vec![
            hist_row("java.util.HashMap$Node", 50_000_000, 2_000_000_000),
            hist_row(
                "java.util.concurrent.ConcurrentHashMap$Node",
                10_000_000,
                400_000_000,
            ),
            hist_row("java.lang.String", 5_000_000, 160_000_000),
        ];
        let s = MapEntryDominance
            .eval(&r)
            .expect("60M map entry objects must fire");
        assert_eq!(s.id, "map-entry-dominance");
        assert!(s.detail.contains("60,000,000"));

        // Below both floor (5M total) and pct (2.5%): silent
        r.overview.histogram[0].instances = 4_000_000;
        r.overview.histogram[1].instances = 1_000_000;
        assert!(MapEntryDominance.eval(&r).is_none());
    }

    #[test]
    fn map_entry_dominance_fires_on_pct() {
        let mut r = base_report();
        r.overview.total_objects = 10_000_000; // small total
        r.overview.histogram = vec![hist_row(
            "java.util.LinkedHashMap$Entry",
            2_500_000, // 25% of total objects — fires on pct even below count floor
            100_000_000,
        )];
        let s = MapEntryDominance
            .eval(&r)
            .expect("25% map entries must fire on pct");
        assert_eq!(s.id, "map-entry-dominance");
        assert!(s.detail.contains("25.0%"));
    }

    #[test]
    fn hibernate_interceptor_fires_on_many_instances() {
        let mut r = base_report();
        r.overview.histogram = vec![
            hist_row(
                "com.sap.engine.services.orpersistence.GenericFieldInterceptor",
                2_000_000,
                64_000_000,
            ),
            hist_row(
                "com.sap.engine.services.orpersistence.SetterInterceptMethodAdaptor",
                1_500_000,
                48_000_000,
            ),
            hist_row("java.lang.String", 500_000, 16_000_000),
        ];
        let s = HibernateInterceptorAccumulation
            .eval(&r)
            .expect("3.5M interceptor instances must fire");
        assert_eq!(s.id, "hibernate-interceptor-accumulation");
        assert!(s.detail.contains("3,500,000"));
        assert!(s.detail.contains("GenericFieldInterceptor"));

        // Below instance floor
        r.overview.histogram[0].instances = 100_000;
        r.overview.histogram[1].instances = 100_000;
        assert!(HibernateInterceptorAccumulation.eval(&r).is_none());

        // Above instance floor but below shallow floor
        r.overview.histogram[0].instances = 1_000_000;
        r.overview.histogram[0].shallow = 1_000; // tiny
        r.overview.histogram[1].instances = 100_000;
        r.overview.histogram[1].shallow = 1_000;
        assert!(HibernateInterceptorAccumulation.eval(&r).is_none());

        // No matching classes: silent
        r.overview.histogram = vec![hist_row("java.lang.String", 5_000_000, 80_000_000)];
        assert!(HibernateInterceptorAccumulation.eval(&r).is_none());
    }

    #[test]
    fn lock_object_proliferation_fires_on_high_count() {
        let mut r = base_report();
        r.overview.histogram = vec![
            hist_row(
                "java.util.concurrent.locks.ReentrantLock$NonfairSync",
                800_000,
                25_600_000,
            ),
            hist_row(
                "java.util.concurrent.locks.ReentrantReadWriteLock$NonfairSync",
                200_000,
                6_400_000,
            ),
        ];
        let s = LockObjectProliferation
            .eval(&r)
            .expect("1M lock objects must fire");
        assert_eq!(s.id, "lock-object-proliferation");
        assert!(s.detail.contains("1,000,000"));

        // Below floor
        r.overview.histogram[0].instances = 100_000;
        r.overview.histogram[1].instances = 50_000;
        assert!(LockObjectProliferation.eval(&r).is_none());

        // Unrelated lock class: silent
        r.overview.histogram = vec![hist_row(
            "java.util.concurrent.locks.AbstractQueuedSynchronizer",
            600_000,
            19_200_000,
        )];
        assert!(LockObjectProliferation.eval(&r).is_none());
    }

    #[test]
    fn perf_monitoring_fires_on_call_graph_accumulation() {
        let mut r = base_report();
        r.overview.histogram = vec![
            hist_row(
                "com.sap.engine.services.perflog.CallStack$CallNode",
                500_000,
                40_000_000,
            ),
            hist_row(
                "com.sap.engine.services.perflog.CallStack",
                100_000,
                8_000_000,
            ),
        ];
        let s = PerfMonitoringRetention
            .eval(&r)
            .expect("600k perf call-graph nodes must fire");
        assert_eq!(s.id, "perf-monitoring-retention");
        assert!(s.detail.contains("600,000"));
        assert!(s.detail.contains("CallNode"));

        // Below floor
        r.overview.histogram[0].instances = 50_000;
        r.overview.histogram[1].instances = 10_000;
        assert!(PerfMonitoringRetention.eval(&r).is_none());

        // Right class name but wrong package (not perf infra): silent
        r.overview.histogram = vec![hist_row("com.example.domain.CallNode", 500_000, 16_000_000)];
        assert!(PerfMonitoringRetention.eval(&r).is_none());

        // No matching classes: silent
        r.overview.histogram = vec![hist_row("java.lang.String", 1_000_000, 32_000_000)];
        assert!(PerfMonitoringRetention.eval(&r).is_none());
    }

    #[test]
    fn threadlocal_value_retention_fires_on_large_retained() {
        use crate::report::model::ThreadLocalLeakRow;
        let mut r = base_report();

        // Empty analysis: silent
        assert!(ThreadLocalValueRetention.eval(&r).is_none());

        r.thread_local_analysis = vec![
            ThreadLocalLeakRow {
                value_class: "com.example.RequestContext".into(),
                entry_count: 800,
                stale_count: 0,
                retained: 50_000_000, // 50 MB
            },
            ThreadLocalLeakRow {
                value_class: "com.example.FormatCache".into(),
                entry_count: 200,
                stale_count: 10,
                retained: 5_000_000,
            },
        ];
        let s = ThreadLocalValueRetention
            .eval(&r)
            .expect("55 MB ThreadLocal retention must fire");
        assert_eq!(s.id, "threadlocal-value-retention");
        assert!(s.detail.contains("RequestContext"));
        assert!(s.detail.contains("800")); // entry count

        // Below floor
        r.thread_local_analysis[0].retained = 1_000_000;
        r.thread_local_analysis[1].retained = 500_000;
        assert!(ThreadLocalValueRetention.eval(&r).is_none());

        // High stale fraction triggers stale note
        r.thread_local_analysis[0].retained = 50_000_000;
        r.thread_local_analysis[0].entry_count = 100;
        r.thread_local_analysis[0].stale_count = 80; // 80% stale
        let s2 = ThreadLocalValueRetention.eval(&r).expect("must fire");
        assert!(s2.detail.contains("stale"));
    }

    #[test]
    fn humongous_object_fires_on_large_array() {
        use crate::report::model::TopArrayRow;
        let mut r = base_report();
        r.overview.total_shallow = 200 * 1024 * 1024;

        // No arrays: silent
        assert!(HumongousObjectAllocation.eval(&r).is_none());

        r.collections.top_prim_arrays.top_individual = vec![TopArrayRow {
            array_class: "byte[]".into(),
            length: 8_000_000,
            shallow: 8 * 1024 * 1024, // 8 MB → fires
            obj_index_1based: 1,
            owner: Some("com.example.ResponseBuffer#buf".into()),
            non_null: None,
        }];
        let s = HumongousObjectAllocation
            .eval(&r)
            .expect("8 MB array must fire");
        assert_eq!(s.id, "humongous-object-allocation");
        assert!(s.detail.contains("byte[]"));
        assert!(s.detail.contains("ResponseBuffer"));

        // Below 4 MB floor: silent
        r.collections.top_prim_arrays.top_individual[0].shallow = 2 * 1024 * 1024;
        assert!(HumongousObjectAllocation.eval(&r).is_none());
    }

    #[test]
    fn component_imbalance_fires_on_dominant_component() {
        use crate::report::model::{Component, TopComponents};
        let mut r = base_report();

        // Fewer than 3 components: silent
        r.top_components = TopComponents {
            components: vec![
                Component {
                    loader_label: "app1".into(),
                    retained: 900,
                    pct: 90.0,
                    top_classes: vec![],
                },
                Component {
                    loader_label: "app2".into(),
                    retained: 100,
                    pct: 10.0,
                    top_classes: vec![],
                },
            ],
        };
        assert!(
            ComponentRetentionImbalance.eval(&r).is_none(),
            "only 2 components: silent"
        );

        r.top_components.components.push(Component {
            loader_label: "system".into(),
            retained: 10,
            pct: 1.0,
            top_classes: vec![],
        });
        // Now 3 components, top at 90% >= 60%: fires
        let s = ComponentRetentionImbalance
            .eval(&r)
            .expect("90% top component must fire");
        assert_eq!(s.id, "component-retention-imbalance");
        assert!(s.detail.contains("app1"));
        assert!(s.detail.contains("app2"));

        // Top at only 50%: silent
        r.top_components.components[0].pct = 50.0;
        assert!(ComponentRetentionImbalance.eval(&r).is_none());
    }

    #[test]
    fn exception_accumulation_fires_and_silent() {
        let mut r = base_report();
        r.overview.total_shallow = 1_000_000_000;

        // Below instance floor: silent.
        r.overview.histogram.push(hist_row(
            "com.example.MyException",
            EXCEPTION_ACCUM_FLOOR - 1,
            EXCEPTION_ACCUM_SHALLOW_FLOOR,
        ));
        assert!(ExceptionObjectAccumulation.eval(&r).is_none());

        // Below shallow floor: silent.
        r.overview.histogram[0] = hist_row(
            "com.example.MyException",
            EXCEPTION_ACCUM_FLOOR,
            EXCEPTION_ACCUM_SHALLOW_FLOOR - 1,
        );
        assert!(ExceptionObjectAccumulation.eval(&r).is_none());

        // Both floors met: fires.
        r.overview.histogram[0] = hist_row(
            "com.example.MyException",
            EXCEPTION_ACCUM_FLOOR,
            EXCEPTION_ACCUM_SHALLOW_FLOOR,
        );
        let s = ExceptionObjectAccumulation
            .eval(&r)
            .expect("both floors met must fire");
        assert_eq!(s.id, "exception-object-accumulation");
        assert!(s.detail.contains("MyException"));

        // Noise suffixes: silent.
        r.overview.histogram[0] = hist_row(
            "com.example.MyErrorCode",
            EXCEPTION_ACCUM_FLOOR,
            EXCEPTION_ACCUM_SHALLOW_FLOOR,
        );
        assert!(ExceptionObjectAccumulation.eval(&r).is_none());

        // Plain "Error" suffix fires.
        r.overview.histogram[0] = hist_row(
            "java.lang.OutOfMemoryError",
            EXCEPTION_ACCUM_FLOOR,
            EXCEPTION_ACCUM_SHALLOW_FLOOR,
        );
        let s2 = ExceptionObjectAccumulation
            .eval(&r)
            .expect("Error suffix must fire");
        assert_eq!(s2.id, "exception-object-accumulation");
        assert!(s2.detail.contains("OutOfMemoryError"));
    }

    #[test]
    fn daemon_thread_retention_fires_and_silent() {
        let mut r = base_report();
        r.overview.total_shallow = 1_000_000_000;

        let daemon = ThreadInfo {
            thread_serial: 1,
            name: Some("background-worker-1".into()),
            class_name: Some("java.lang.Thread".into()),
            is_daemon: true,
            retained: 200_000_000, // 20% of 1 GB
            ..ThreadInfo::default()
        };

        // Non-daemon with same retained: silent.
        let mut non_daemon = daemon.clone();
        non_daemon.is_daemon = false;
        non_daemon.thread_serial = 2;
        r.threads.threads = vec![non_daemon];
        assert!(DaemonThreadRetention.eval(&r).is_none());

        // Daemon but below absolute floor: silent.
        let mut small_daemon = daemon.clone();
        small_daemon.retained = DAEMON_RETAINED_FLOOR - 1;
        r.threads.threads = vec![small_daemon];
        assert!(DaemonThreadRetention.eval(&r).is_none());

        // Daemon meets both floors: fires.
        r.threads.threads = vec![daemon];
        let s = DaemonThreadRetention
            .eval(&r)
            .expect("daemon with 20% retained must fire");
        assert_eq!(s.id, "daemon-thread-retention");
        assert!(s.detail.contains("background-worker-1"));
        assert!(s.detail.contains("20.0%") || s.detail.contains("20%"));

        // At exactly DAEMON_RETAINED_PCT boundary but below: silent.
        let pct_boundary = ((DAEMON_RETAINED_PCT - 0.1) / 100.0 * 1_000_000_000f64) as u64;
        r.threads.threads[0].retained = pct_boundary;
        assert!(DaemonThreadRetention.eval(&r).is_none());
    }
}

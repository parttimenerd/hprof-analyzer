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
    ]
}

/// Evaluate every rule once, in registry order, collecting the ones that fire.
pub fn evaluate_triage(r: &Report) -> Vec<TriageSignal> {
    rules().iter().filter_map(|rule| rule.eval(r)).collect()
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
    }
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
            Some(signal(
                "headline-retainer",
                TriageSeverity::Critical,
                "Headline retainer",
                format!(
                    "`{}` ({}) retains {} ({:.1}% of reachable heap).",
                    s.pretty_class,
                    kind,
                    format_bytes(s.retained),
                    pct_of(s.retained, total),
                ),
                Some(("leak-suspects", "Leak Suspects")),
            ))
        } else if let Some(o) = r.top.biggest_objects.first() {
            Some(signal(
                "headline-retainer",
                TriageSeverity::Warning,
                "Headline retainer",
                format!(
                    "`{}` retains {} ({:.1}% of reachable heap).",
                    o.display_class,
                    format_bytes(o.retained),
                    pct_of(o.retained, total),
                ),
                Some(("top-consumers", "Top Consumers")),
            ))
        } else {
            Some(signal(
                "headline-retainer",
                TriageSeverity::Info,
                "Headline retainer",
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
                signal(
                    "concentration",
                    TriageSeverity::Critical,
                    "Concentration",
                    format!(
                        "highly concentrated — `{}` ({}){} holds {:.1}% of the heap, so freeing it would reclaim most memory.",
                        s.pretty_class,
                        kind,
                        held_by,
                        pct_of(s.retained, total),
                    ),
                    Some(("leak-suspects", "Leak Suspects")),
                )
            }
            Some(_) => signal(
                "concentration",
                TriageSeverity::Info,
                "Concentration",
                "diffuse — retention is spread across multiple roots, so there is no single object to free.".to_string(),
                Some(("leak-suspects", "Leak Suspects")),
            ),
            None => signal(
                "concentration",
                TriageSeverity::Info,
                "Concentration",
                "diffuse — no suspect exceeds the threshold; retention is spread across many roots.".to_string(),
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
            "Dominant GC-root type",
            format!(
                "{:.1}% of the heap is held by \"{}\" roots — retention concentrates at one root class.",
                pct, top.root_type,
            ),
            Some(("overview", "System Overview")),
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
            "deep (retention flows through long dominator chains — often nested collections or linked structures)"
        };
        Some(signal(
            "shape",
            TriageSeverity::Info,
            "Shape",
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
        let detail = match r
            .top
            .biggest_objects
            .first()
            .map(|o| match o.owner.as_deref() {
                Some(owner) => format!("`{}` (held by `{}`)", o.display_class, owner),
                None => format!("`{}`", o.display_class),
            }) {
            Some(name) => format!(
                "the single biggest object, {}, retains {:.1}% and the top 10 retain {:.1}% of the heap; {} object(s) each hold >=1%.",
                name, top1_pct, top10_pct, rc.num_objects_ge_1pct,
            ),
            None => format!(
                "the single biggest object retains {:.1}% and the top 10 retain {:.1}% of the heap; {} object(s) each hold >=1%.",
                top1_pct, top10_pct, rc.num_objects_ge_1pct,
            ),
        };
        Some(signal(
            "one-leak-or-many",
            TriageSeverity::Info,
            "One leak or many",
            detail,
            Some(("top-consumers", "Top Consumers")),
        ))
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
        Some(signal(
            "classloader-leak",
            TriageSeverity::Warning,
            "Classloader leak",
            format!(
                "`{}` is loaded by {} class loaders ({} retained) — classic reload leak.",
                dup.pretty_class,
                dup.loader_count,
                format_bytes(dup.total_retained),
            ),
            Some(("duplicate-classes", "Duplicate Classes")),
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
            "ThreadLocal leak",
            format!(
                "{} ThreadLocalMap entries have a cleared key — abandoned thread-local values that will never be reclaimed.",
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
            "Thread pinning",
            format!(
                "thread `{}` retains {} ({:.1}% of heap) and pins {} thread-local roots — a live thread is holding memory alive.",
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
        let only_weak: u64 = [&refs.soft, &refs.weak, &refs.phantom]
            .into_iter()
            .flatten()
            .flat_map(|s| s.only_weakly_retained.iter())
            .map(|row| row.objects)
            .sum();
        if only_weak < WEAKREF_FLOOR {
            return None;
        }
        Some(signal(
            "weak-ref-escape",
            TriageSeverity::Info,
            "Weak-ref escape",
            format!(
                "{} objects are reachable only via soft/weak/phantom references — likely reclaimable under memory pressure.",
                fmt_count(only_weak),
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
            "Proxy/lambda bloat",
            format!(
                "{} of {} loaded classes ({:.1}%) are anonymous/generated (lambda/proxy) — possible classloader churn.",
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
            "Off-heap (DirectByteBuffer)",
            format!(
                "{} of native memory is held by live DirectByteBuffers — not counted in heap size but can dominate RSS.",
                format_bytes(cap),
            ),
            Some(("leak-indicators", "Leak Indicators")),
        ))
    }
}

/// GC waste. Reads `overview.heap_fragmentation_ratio`, `unreachable_shallow`,
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
        Some(signal(
            "gc-waste",
            TriageSeverity::Warning,
            "GC waste",
            format!(
                "{:.1}% of the heap is unreachable garbage ({} shallow, {} retained){}.",
                pct,
                format_bytes(o.unreachable_shallow),
                format_bytes(o.unreachable_retained),
                cluster,
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
            "Over-capacity collections",
            format!(
                "{} wasted by under-filled collections (<=50% full across {} tracked) — oversized backing arrays.",
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
        Some(signal(
            "constant-value-arrays",
            TriageSeverity::Info,
            "Constant-value arrays",
            format!(
                "{} in single-value primitive arrays; biggest group `{}` × {} instances — likely zero-filled/uninitialized waste.",
                format_bytes(sum),
                big.array_class,
                fmt_count(big.objects),
            ),
            Some(("collections", "Collections")),
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
        Some(signal(
            "object-swarm",
            TriageSeverity::Warning,
            "Object swarm",
            format!(
                "{} live `{}` instances ({} shallow, {:.1}% of heap) — typically an unbounded queue, list, or log accumulation.",
                fmt_count(row.instances),
                row.pretty_class,
                format_bytes(row.shallow),
                pct_of(row.shallow, total),
            ),
            Some(("overview", "System Overview")),
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
            "Boxed-primitive bloat",
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
            "Classloader explosion",
            format!(
                "{} live ClassLoader instances — abnormally high; typical apps use tens. Likely a dynamic-class or redeploy leak.",
                fmt_count(n),
            ),
            Some(("overview", "System Overview")),
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
            "Thread swarm",
            format!(
                "{} live threads retaining {} in aggregate — likely unbounded thread creation or a leaking thread pool.",
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
            "Duplicate strings",
            format!(
                "~{} wasted by {} duplicated String values ({} total instances){}.",
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
            "Char-array slack",
            format!(
                "~{} slack in {} over-allocated char[]/byte[] String backing arrays — possible `substring`/`StringBuilder` waste.",
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
        Some(signal(
            "large-unbounded-collection",
            TriageSeverity::Warning,
            "Large unbounded collection",
            format!(
                "one `{}` holds {} elements{}{}  — likely a static or unbounded cache that never evicts.",
                row.container_class,
                fmt_count(row.elements),
                retained_str,
                owner_str,
            ),
            Some(("biggest-collections", "Biggest Collections")),
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
            "Finalizer queue backlog",
            format!(
                "{} live `java.lang.ref.Finalizer` instances — the finalizer thread is falling behind; finalizeable objects (e.g. `Deflater`, JDBC connections) accumulate until drained.",
                fmt_count(row.instances),
            ),
            Some(("overview", "System Overview")),
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
            "Metaspace pressure",
            format!(
                "{} classes loaded — far above normal; class metadata is likely exhausting Metaspace. Typical cause: CGLIB/Byte Buddy/Groovy proxy generation without caching.",
                fmt_count(n),
            ),
            Some(("overview", "System Overview")),
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
            "Cached reflection metadata",
            format!(
                "{} live `java.lang.reflect.{{Method,Field,Constructor}}` objects — framework reflection caches are unbounded (typically Spring/Hibernate accumulating per scanned class).",
                fmt_count(total),
            ),
            Some(("overview", "System Overview")),
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
            "JNI global-reference leak",
            format!(
                "{} JNI Global roots retaining {} ({:.1}% of heap) — native code is accumulating global references without releasing them; audit `JNI_DeleteGlobalRef` call sites.",
                fmt_count(count),
                format_bytes(retained),
                pct_of(retained, total),
            ),
            Some(("overview", "System Overview")),
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
        Some(signal(
            "heap-composition-skew",
            TriageSeverity::Info,
            "Heap composition skew",
            format!(
                "`{}` account for {:.1}% of reachable heap — the heap is bulk-data dominated; most memory is in raw buffers rather than object graphs.",
                dominant.kind, pct,
            ),
            Some(("overview", "System Overview")),
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
        Some(signal(
            "static-field-anchor",
            TriageSeverity::Warning,
            "Static-field anchor",
            format!(
                "`{}` is anchored via a static field (`Sticky Class` root) and retains {} ({:.1}% of heap) — this object lives for the classloader lifetime and is never evicted.",
                s.pretty_class,
                format_bytes(s.retained),
                pct,
            ),
            Some(("leak-suspects", "Leak Suspects")),
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
        Some(signal(
            "session-scope-leak",
            TriageSeverity::Warning,
            "Session-scope leak",
            format!(
                "{} live `{}` instances — session objects are accumulating; a registry is holding sessions that were never invalidated.",
                fmt_count(row.instances),
                row.pretty_class,
            ),
            Some(("overview", "System Overview")),
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
        Some(signal(
            "connection-leak",
            TriageSeverity::Warning,
            "Connection / socket leak",
            format!(
                "{} live `{}` objects — exceeds any reasonable pool or connection limit; connections are likely being acquired without `close()`.",
                fmt_count(row.instances),
                row.pretty_class,
            ),
            Some(("overview", "System Overview")),
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
        Some(signal(
            "event-listener-accumulation",
            TriageSeverity::Warning,
            "Event-listener accumulation",
            format!(
                "{} live `{}` instances — listeners are accumulating without removal; the publisher is keeping them alive indefinitely.",
                fmt_count(row.instances),
                row.pretty_class,
            ),
            Some(("overview", "System Overview")),
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
        Some(signal(
            "parser-output-accumulation",
            TriageSeverity::Info,
            "Parser-output accumulation",
            format!(
                "{} live `{}` instances — XML/JSON parse results are accumulating; parsed documents are not being discarded after processing.",
                fmt_count(row.instances),
                row.pretty_class,
            ),
            Some(("overview", "System Overview")),
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
            "Interned-string bloat",
            format!(
                "{} live `java.lang.String` instances with {} JNI Global roots — `String.intern()` may be called on dynamic values, causing the intern table to grow without bound.",
                fmt_count(string_count),
                fmt_count(jni_global_count),
            ),
            Some(("overview", "System Overview")),
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
            "Sparse object arrays",
            format!(
                "{} object arrays are <={}% full ({} wasted on null slots) — sparse or multi-dimensional array structures consuming excess memory.",
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
        Some(signal(
            "big-drop-concentration",
            TriageSeverity::Critical,
            "Dominator-tree big drop",
            format!(
                "`{}` is the single largest memory bucket: {:.1}% ({}) of the heap \
                 drops here in the dominator tree — almost all its retained memory \
                 is not shared with any other top-level subtree.",
                row.display_class,
                pct,
                format_bytes(row.drop_bytes),
            ),
            Some(("dominator-tree", "Dominator Tree")),
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
        let overhead = r.overview.total_objects * header_bytes;
        let pct = overhead as f64 / total as f64 * 100.0;
        if pct < HEADER_OVERHEAD_PCT {
            return None;
        }
        Some(signal(
            "fixed-per-object-overhead",
            TriageSeverity::Warning,
            "Fixed per-object header overhead",
            format!(
                "{} objects × {} B header = {} ({:.1}% of heap) is consumed by JVM \
                 object headers alone — consider value types, primitive arrays, or \
                 fewer wrapper objects.",
                fmt_count(r.overview.total_objects),
                header_bytes,
                format_bytes(overhead),
                pct,
            ),
            Some(("header-overhead", "Header Overhead")),
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
            "Hash-map collision hotspot",
            format!(
                "{} of {} tracked maps ({:.1}%) have a load factor > {}% — \
                 over-packed hash tables cause long collision chains and degrade \
                 lookup performance.",
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
            "Empty-collection cemetery",
            format!(
                "{} of {} tracked collections ({:.1}%) are empty (size == 0) — \
                 pre-allocated but never populated containers waste object-header \
                 overhead; consider lazy initialisation or null.",
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
        Some(signal(
            "oversized-prim-array",
            TriageSeverity::Warning,
            "Oversized primitive array",
            format!(
                "A single `{}` ({} elements, {}){} accounts for {:.1}% of the heap — \
                 consider chunking, memory-mapping, or off-heap storage.",
                row.array_class,
                fmt_count(row.length),
                format_bytes(row.shallow),
                owner_clause,
                pct,
            ),
            Some(("arrays", "Arrays")),
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
            "Duplicate primitive arrays",
            format!(
                "{} ({:.1}% of heap) wasted by content-identical primitive arrays — \
                 multiple copies of the same byte[]/int[]/etc. payload could be \
                 deduplicated or replaced with a shared constant.",
                format_bytes(wasted),
                pct_of(wasted, total),
            ),
            Some(("dup-strings", "Duplicate Strings")),
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
        assert_eq!(s.anchor.as_deref(), Some("leak-indicators"));
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
            loader_id: 0,
            loader_label: None,
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
            loader_id: 0,
            loader_label: None,
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
            loader_id: 0,
            loader_label: None,
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
                kind: "Primitive arrays".into(),
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
        assert!(s.detail.contains("Primitive arrays"));

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
        r.overview.duplicate_prim_arrays.as_mut().unwrap().total_wasted_bytes = 1024;
        assert!(DuplicatePrimArrays.eval(&r).is_none());
    }
}

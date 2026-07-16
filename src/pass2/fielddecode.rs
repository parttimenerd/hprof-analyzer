//! Always-on field-decode deep-scan: computes five collection/array views
//! (collection fill ratio, collections-by-size histogram, object-array fill
//! ratio, map-collision proxy, constant primitive arrays) and three reference
//! views (soft/weak/phantom referent statistics) in ONE shared full-file scan
//! (instances, primitive arrays, and object arrays fused via `scan_all_records`).
//!
//! Every aggregate is bounded by an explicit cap (see the consts below) so RSS
//! stays within the grant on multi-GB dumps: no per-object Vec is ever retained.
//! Unknown/missing fields resolve to `None` via [`field_offset`] and the
//! collection is silently skipped (graceful, never panics).

use std::collections::HashMap;

use crate::{
    pass1::Pass1,
    report::{
        ArrayFillRatio, CollectionFillRatio, CollectionsAnalysis, CollectionsBySize,
        ConstantArrayRow, ConstantPrimitiveArrays, FillRatioBucket, MapCollisionRatio,
        RefStatClassRow, ReferenceStats, ReferencesAnalysis, SizeHistogramBucket,
    },
    types::HprofType,
};

use super::{
    AttributionRaw, Record, field_offset, prim_array_class_name, read_ref, scan_all_records,
};

// ── Caps (bound every aggregate) ─────────────────────────────────────────────

/// Max distinct backing-array addresses tracked for the collection fill-ratio /
/// map-collision folds. Beyond this the fill views stop growing `tracked`
/// (`total` keeps counting), so RSS is O(WANTED_CAP) entries.
const WANTED_CAP: usize = 1_500_000;
/// Max distinct (type,len,value) groups tracked for constant primitive arrays.
/// Beyond this remaining groups fold into one "other" row (truncated=true).
const CONST_ARRAY_CAP: usize = 100_000;
/// Max referent object indices pushed per reference kind for the later
/// only-weakly-retained computation.
const REFERENT_CAP: usize = 1_000_000;
/// Max distinct referent classes retained per reference kind's histogram.
const REFERENT_HIST_CAP: usize = 200;

/// Max holder→pointee edges collected under --collections (16 B each → 160 MB).
const FIELD_REF_CAP: usize = 10_000_000;
/// Max container records collected under --collections (~32 B each → ~48 MB).
const CONTAINER_CAP: usize = 1_500_000;
/// Fixed top-N for both attribution rankings (documented, used by AREA C).
pub(crate) const ATTRIBUTION_TOP_N: usize = 25;

// ── Collection descriptor table ──────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum CollKind {
    List,
    Map,
    Set,
    Deque,
    Queue,
    Tree,
}

impl CollKind {
    /// Container-kind discriminant used in [`ContainerRecord::kind`] /
    /// `AttributionRaw::container_kind` (0=List..5=Tree; 6/7 reserved for
    /// object/primitive arrays). Widening the value space here does NOT touch
    /// the serialized schema.
    fn discriminant(self) -> u8 {
        match self {
            CollKind::List => 0,
            CollKind::Map => 1,
            CollKind::Set => 2,
            CollKind::Deque => 3,
            CollKind::Queue => 4,
            CollKind::Tree => 5,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct CollDesc {
    pub(crate) class_name: String,
    /// (field_name, declaring_owner_class) for the element-count field.
    pub(crate) size_field: Option<(String, String)>,
    /// (field_name, declaring_owner_class) for the backing object-array field.
    pub(crate) array_field: Option<(String, String)>,
    /// (field_name, declaring_owner_class) for a nested delegate collection
    /// (e.g. HashSet.map -> a HashMap); resolved as an object reference and
    /// currently used only for classification (Set/Tree wrappers).
    #[allow(dead_code)]
    pub(crate) nested_map_field: Option<(String, String)>,
    pub(crate) kind: CollKind,
}

/// Helper macro to construct a CollDesc with owned String fields.
macro_rules! cd {
    (
        $class_name:expr,
        size: ($sf:expr, $so:expr),
        arr: ($af:expr, $ao:expr),
        nested: ($nf:expr, $no:expr),
        $kind:expr
    ) => {
        CollDesc {
            class_name: $class_name.to_string(),
            size_field: Some(($sf.to_string(), $so.to_string())),
            array_field: Some(($af.to_string(), $ao.to_string())),
            nested_map_field: Some(($nf.to_string(), $no.to_string())),
            kind: $kind,
        }
    };
    (
        $class_name:expr,
        size: ($sf:expr, $so:expr),
        arr: ($af:expr, $ao:expr),
        $kind:expr
    ) => {
        CollDesc {
            class_name: $class_name.to_string(),
            size_field: Some(($sf.to_string(), $so.to_string())),
            array_field: Some(($af.to_string(), $ao.to_string())),
            nested_map_field: None,
            kind: $kind,
        }
    };
    (
        $class_name:expr,
        size: ($sf:expr, $so:expr),
        $kind:expr
    ) => {
        CollDesc {
            class_name: $class_name.to_string(),
            size_field: Some(($sf.to_string(), $so.to_string())),
            array_field: None,
            nested_map_field: None,
            kind: $kind,
        }
    };
    (
        $class_name:expr,
        arr: ($af:expr, $ao:expr),
        $kind:expr
    ) => {
        CollDesc {
            class_name: $class_name.to_string(),
            size_field: None,
            array_field: Some(($af.to_string(), $ao.to_string())),
            nested_map_field: None,
            kind: $kind,
        }
    };
    (
        $class_name:expr,
        nested: ($nf:expr, $no:expr),
        $kind:expr
    ) => {
        CollDesc {
            class_name: $class_name.to_string(),
            size_field: None,
            array_field: None,
            nested_map_field: Some(($nf.to_string(), $no.to_string())),
            kind: $kind,
        }
    };
    (
        $class_name:expr,
        $kind:expr
    ) => {
        CollDesc {
            class_name: $class_name.to_string(),
            size_field: None,
            array_field: None,
            nested_map_field: None,
            kind: $kind,
        }
    };
}

/// Return the built-in collection descriptors. HPROF class names use `/`
/// separators (e.g. `java/util/HashMap`), matching `field_offset`'s expectation.
pub(crate) fn builtin_coll_descs() -> Vec<CollDesc> {
    vec![
        // ── JDK ──────────────────────────────────────────────────────────────
        cd!("java/util/HashMap",
            size: ("size", "java/util/HashMap"),
            arr:  ("table", "java/util/HashMap"),
            CollKind::Map),
        cd!("java/util/LinkedHashMap",
            // size/table declared on java.util.HashMap (LinkedHashMap extends it).
            size: ("size", "java/util/HashMap"),
            arr:  ("table", "java/util/HashMap"),
            CollKind::Map),
        cd!("java/util/Hashtable",
            size: ("count", "java/util/Hashtable"),
            arr:  ("table", "java/util/Hashtable"),
            CollKind::Map),
        // size is not a plain field; approximate from non-null slots in scan 3.
        cd!("java/util/concurrent/ConcurrentHashMap",
            arr: ("table", "java/util/concurrent/ConcurrentHashMap"),
            CollKind::Map),
        cd!("java/util/TreeMap",
            size: ("size", "java/util/TreeMap"),
            CollKind::Tree),
        cd!("java/util/ArrayList",
            size: ("size", "java/util/ArrayList"),
            arr:  ("elementData", "java/util/ArrayList"),
            CollKind::List),
        cd!("java/util/Vector",
            size: ("elementCount", "java/util/Vector"),
            arr:  ("elementData", "java/util/Vector"),
            CollKind::List),
        cd!("java/util/LinkedList",
            size: ("size", "java/util/LinkedList"),
            CollKind::List),
        cd!("java/util/ArrayDeque",
            arr: ("elements", "java/util/ArrayDeque"),
            CollKind::Deque),
        cd!("java/util/HashSet",
            nested: ("map", "java/util/HashSet"),
            CollKind::Set),
        cd!("java/util/TreeSet",
            nested: ("m", "java/util/TreeSet"),
            CollKind::Set),
        // Kotlin's standard collections (kotlin.collections.ArrayList / HashMap /
        // LinkedHashMap) are thin aliases for the JDK types and match via the
        // super-chain entries above — no dedicated rows are needed here.

        // ── Scala ─────────────────────────────────────────────────────────────
        cd!("scala/collection/mutable/HashMap",
            size: ("contentSize", "scala/collection/mutable/HashMap"),
            arr:  ("table", "scala/collection/mutable/HashMap"),
            CollKind::Map),
        cd!("scala/collection/mutable/ArrayBuffer",
            size: ("size0", "scala/collection/mutable/ArrayBuffer"),
            arr:  ("array", "scala/collection/mutable/ArrayBuffer"),
            CollKind::List),
        // ── Eclipse Collections ───────────────────────────────────────────────
        cd!("org/eclipse/collections/impl/map/mutable/UnifiedMap",
            size: ("occupied", "org/eclipse/collections/impl/map/mutable/UnifiedMap"),
            arr:  ("table",    "org/eclipse/collections/impl/map/mutable/UnifiedMap"),
            CollKind::Map),
        cd!("org/eclipse/collections/impl/list/mutable/FastList",
            size: ("size",  "org/eclipse/collections/impl/list/mutable/FastList"),
            arr:  ("items", "org/eclipse/collections/impl/list/mutable/FastList"),
            CollKind::List),
        cd!("org/eclipse/collections/impl/set/mutable/UnifiedSet",
            size: ("occupied", "org/eclipse/collections/impl/set/mutable/UnifiedSet"),
            arr:  ("table",    "org/eclipse/collections/impl/set/mutable/UnifiedSet"),
            CollKind::Set),
        // ── Trove (modern gnu/trove/{map,set}/hash/* layout) ─────────────────
        // _size is the element count; _set is the backing Object[] for hash containers.
        cd!("gnu/trove/map/hash/THashMap",
            size: ("_size", "gnu/trove/impl/hash/THash"),
            arr:  ("_set",  "gnu/trove/impl/hash/TObjectHash"),
            CollKind::Map),
        cd!("gnu/trove/set/hash/THashSet",
            size: ("_size", "gnu/trove/impl/hash/THash"),
            arr:  ("_set",  "gnu/trove/impl/hash/TObjectHash"),
            CollKind::Set),
        cd!("gnu/trove/map/hash/TIntObjectHashMap",
            size: ("_size",   "gnu/trove/impl/hash/THash"),
            arr:  ("_values", "gnu/trove/map/hash/TIntObjectHashMap"),
            CollKind::Map),
        // ── Trove (legacy flat gnu/trove/* layout) ────────────────────────────
        cd!("gnu/trove/THashMap",
            size: ("_size", "gnu/trove/THash"),
            arr:  ("_set",  "gnu/trove/TObjectHash"),
            CollKind::Map),
        cd!("gnu/trove/THashSet",
            size: ("_size", "gnu/trove/THash"),
            arr:  ("_set",  "gnu/trove/TObjectHash"),
            CollKind::Set),
        // ── Guava ─────────────────────────────────────────────────────────────
        cd!("com/google/common/collect/ImmutableList",
            arr: ("array", "com/google/common/collect/ImmutableList"),
            CollKind::List),
        cd!("com/google/common/collect/ImmutableMap",
            arr: ("table", "com/google/common/collect/ImmutableMap"),
            CollKind::Map),
        cd!("com/google/common/collect/ImmutableSet",
            arr: ("elements", "com/google/common/collect/ImmutableSet"),
            CollKind::Set),
        cd!("com/google/common/collect/ImmutableMultimap", CollKind::Map),
        cd!("com/google/common/collect/ArrayListMultimap",
            nested: ("map", "com/google/common/collect/ArrayListMultimap"),
            CollKind::Map),
        cd!("com/google/common/collect/HashMultimap",
            nested: ("map", "com/google/common/collect/HashMultimap"),
            CollKind::Set),
        cd!("com/google/common/collect/LinkedHashMultimap",
            nested: ("map", "com/google/common/collect/LinkedHashMultimap"),
            CollKind::Map),
        cd!("com/google/common/collect/TreeMultimap",
            nested: ("map", "com/google/common/collect/TreeMultimap"),
            CollKind::Map),
        cd!("com/google/common/collect/HashBiMap",
            size: ("size",           "com/google/common/collect/HashBiMap"),
            arr:  ("hashTableKToV",  "com/google/common/collect/HashBiMap"),
            CollKind::Map),
    ]
}

/// Reference class names, indexed 0=soft, 1=weak, 2=phantom.
static REF_CLASSES: [&str; 3] = [
    "java/lang/ref/SoftReference",
    "java/lang/ref/WeakReference",
    "java/lang/ref/PhantomReference",
];

// ── Per-class memoized classifier ────────────────────────────────────────────

/// Resolved role of an instance's class, computed at most once per distinct
/// class-object address (there are only thousands of classes, so this is
/// bounded).
#[derive(Clone)]
enum ClassRole {
    /// Nothing to decode for this class.
    Plain,
    Collection {
        desc_idx: usize,
        size_off: Option<(u32, HprofType)>,
        array_off: Option<(u32, HprofType)>,
    },
    Reference {
        kind_idx: usize,
        referent_off: (u32, HprofType),
    },
}

/// Walk `class_id`'s super-chain (child-first) and return the first non-None
/// result of `f` applied to each class's HPROF name, or None if the chain is
/// exhausted. Shared by the collection- and reference-class matchers.
fn walk_superchain<T>(
    class_id: u64,
    class_map: &HashMap<u64, crate::pass1::ClassInfo>,
    strings: &HashMap<u64, String>,
    mut f: impl FnMut(&str) -> Option<T>,
) -> Option<T> {
    let mut cur = class_id;
    loop {
        let ci = class_map.get(&cur)?;
        let cname = strings.get(&ci.name_id).map(|s| s.as_str()).unwrap_or("");
        if let Some(hit) = f(cname) {
            return Some(hit);
        }
        if ci.super_id == 0 {
            return None;
        }
        cur = ci.super_id;
    }
}

/// Walk `class_id`'s super-chain (child-first) and return the index of the
/// FIRST CollDesc in `descs` whose class_name matches a class in the chain, or None.
fn match_coll_desc(
    class_id: u64,
    class_map: &HashMap<u64, crate::pass1::ClassInfo>,
    strings: &HashMap<u64, String>,
    descs: &[CollDesc],
) -> Option<usize> {
    walk_superchain(class_id, class_map, strings, |cname| {
        descs.iter().position(|d| d.class_name == cname)
    })
}

/// Walk `class_id`'s super-chain (child-first) and return the ref-kind index
/// (0 soft / 1 weak / 2 phantom) of the FIRST matching REF_CLASSES entry, else
/// None.
fn match_ref_kind(
    class_id: u64,
    class_map: &HashMap<u64, crate::pass1::ClassInfo>,
    strings: &HashMap<u64, String>,
) -> Option<usize> {
    walk_superchain(class_id, class_map, strings, |cname| {
        REF_CLASSES.iter().position(|&r| r == cname)
    })
}

/// Classify a class-object address once, resolving the field offsets it needs.
fn classify(
    class_id: u64,
    class_map: &HashMap<u64, crate::pass1::ClassInfo>,
    strings: &HashMap<u64, String>,
    obj_ref_width: usize,
    descs: &[CollDesc],
) -> ClassRole {
    // References take priority (a Reference is never a collection).
    if let Some(kind_idx) = match_ref_kind(class_id, class_map, strings) {
        if let Some(referent_off) = field_offset(
            class_id,
            "referent",
            "java/lang/ref/Reference",
            class_map,
            strings,
            obj_ref_width,
        ) {
            return ClassRole::Reference {
                kind_idx,
                referent_off,
            };
        }
        // referent field missing → cannot decode; treat as plain.
        return ClassRole::Plain;
    }
    if let Some(desc_idx) = match_coll_desc(class_id, class_map, strings, descs) {
        let desc = &descs[desc_idx];
        let size_off = desc.size_field.as_ref().and_then(|(name, owner)| {
            field_offset(class_id, name, owner, class_map, strings, obj_ref_width)
        });
        let array_off = desc.array_field.as_ref().and_then(|(name, owner)| {
            field_offset(class_id, name, owner, class_map, strings, obj_ref_width)
        });
        // Only track collections we can extract SOMETHING useful from (a size
        // for the size histogram, or a backing array for the fill ratio).
        if size_off.is_some() || array_off.is_some() {
            return ClassRole::Collection {
                desc_idx,
                size_off,
                array_off,
            };
        }
        return ClassRole::Plain;
    }
    ClassRole::Plain
}

// ── Bucketing helpers (pure, unit-tested) ────────────────────────────────────

/// 11 fixed fill-ratio buckets, in basis points. The last bucket is the
/// (9000,10000] band; anything >100% is clamped into it.
const RATIO_BOUNDS: [(u32, u32); 11] = [
    (0, 1000),
    (1000, 2000),
    (2000, 3000),
    (3000, 4000),
    (4000, 5000),
    (5000, 6000),
    (6000, 7000),
    (7000, 8000),
    (8000, 9000),
    (9000, 10000),
    (10000, 10000), // exactly-full sentinel band (used == capacity)
];

/// Map (used, capacity) to a fill-ratio bucket index in RATIO_BOUNDS. Ratio is
/// used/capacity in basis points (0..=10000, clamped). A ratio landing exactly
/// on a bound goes to the LOWER bucket (half-open `(lower, upper]` bands) except
/// ratio 0 which is bucket 0 and ratio 10000 which is the last bucket.
fn ratio_bucket_index(used: u64, capacity: u64) -> usize {
    if capacity == 0 {
        return 0;
    }
    // basis points, clamped to [0, 10000].
    let bp = ((used.saturating_mul(10000)) / capacity).min(10000) as u32;
    if bp >= 10000 {
        return RATIO_BOUNDS.len() - 1;
    }
    // Find the band whose (lower, upper] contains bp; bp==0 → band 0.
    for (i, &(lower, upper)) in RATIO_BOUNDS.iter().enumerate() {
        if bp == 0 {
            return 0;
        }
        if bp > lower && bp <= upper {
            return i;
        }
    }
    0
}

/// Power-of-two upper bound for a length (inclusive). len 0 → 1; len in 5..=8 → 8.
fn size_hist_upper(len: u64) -> u64 {
    if len <= 1 {
        1
    } else {
        len.checked_next_power_of_two().unwrap_or(u64::MAX)
    }
}

// ── Fold accumulators ────────────────────────────────────────────────────────

/// 11-bucket fill-ratio accumulator (objects/shallow/wasted).
#[derive(Default)]
struct FillAcc {
    objects: [u64; 11],
    shallow: [u64; 11],
    wasted: [u64; 11],
}

impl FillAcc {
    fn add(&mut self, used: u64, capacity: u64, shallow: u64, wasted: u64) {
        let i = ratio_bucket_index(used, capacity);
        self.objects[i] += 1;
        self.shallow[i] += shallow;
        self.wasted[i] += wasted;
    }
    fn into_buckets(self) -> Vec<FillRatioBucket> {
        RATIO_BOUNDS
            .iter()
            .enumerate()
            .map(|(i, &(lower, upper))| FillRatioBucket {
                lower_ratio_bp: lower,
                upper_ratio_bp: upper,
                objects: self.objects[i],
                shallow: self.shallow[i],
                wasted: self.wasted[i],
            })
            .collect()
    }
}

/// What we remember about one wanted backing array between scan 1 and scan 3.
struct ArrayWant {
    size: u64,
    is_map: bool,
    /// Shallow size of the COLLECTION instance (not the backing array), carried
    /// from scan 1 so scan 3 can attribute it to the collection fill views.
    coll_shallow: u64,
}

/// Number of individual arrays and array classes surfaced per category.
const TOP_ARRAYS_N: usize = 10;

/// One candidate for the individual top-arrays min-heap. Ordered by shallow so
/// the heap's smallest (via `Reverse`) is the eviction target. `class_key` is
/// the array's class identity (elem type code for prim, array-class object id
/// for obj) so names can be resolved at assembly WITHOUT the freed `class_ids`.
#[derive(Clone, Copy, PartialEq, Eq)]
struct TopArrayCand {
    shallow: u64,
    obj_index: u32,
    length: u64,
    class_key: u64,
}
impl PartialOrd for TopArrayCand {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for TopArrayCand {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Order by shallow, then obj_index for a total, deterministic order.
        self.shallow
            .cmp(&other.shallow)
            .then(self.obj_index.cmp(&other.obj_index))
    }
}

/// Accumulates the top individual arrays (bounded min-heap) and per-class
/// aggregates (map keyed by the array class identity, bounded by #array
/// classes) for one array category (primitive OR object). No per-object Vec is
/// retained.
#[derive(Default)]
struct TopArrayAcc {
    /// Min-heap of the largest arrays by shallow; capped at TOP_ARRAYS_N.
    heap: std::collections::BinaryHeap<std::cmp::Reverse<TopArrayCand>>,
    /// class identity → (objects, aggregate shallow).
    by_class: HashMap<u64, (u64, u64)>,
}
impl TopArrayAcc {
    fn add(&mut self, obj_index: u32, class_key: u64, length: u64, shallow: u64) {
        let e = self.by_class.entry(class_key).or_insert((0, 0));
        e.0 += 1;
        e.1 += shallow;

        let cand = TopArrayCand {
            shallow,
            obj_index,
            length,
            class_key,
        };
        if self.heap.len() < TOP_ARRAYS_N {
            self.heap.push(std::cmp::Reverse(cand));
        } else if let Some(std::cmp::Reverse(min)) = self.heap.peek() {
            if cand > *min {
                self.heap.pop();
                self.heap.push(std::cmp::Reverse(cand));
            }
        }
    }
    /// `name_of(class_key, p1)` resolves the array class name — differs for
    /// primitive vs object arrays (see call sites).
    fn into_top_arrays(
        self,
        p1: &Pass1,
        name_of: impl Fn(u64, &Pass1) -> String,
    ) -> crate::report::TopArrays {
        // Individual: drain heap, resolve names, sort shallow desc / class asc /
        // index asc.
        let mut individual: Vec<crate::report::TopArrayRow> = self
            .heap
            .into_iter()
            .map(|std::cmp::Reverse(c)| crate::report::TopArrayRow {
                array_class: name_of(c.class_key, p1),
                length: c.length,
                shallow: c.shallow,
                obj_index_1based: c.obj_index as u64 + 1,
            })
            .collect();
        individual.sort_by(|a, b| {
            b.shallow
                .cmp(&a.shallow)
                .then_with(|| a.array_class.cmp(&b.array_class))
                .then_with(|| a.obj_index_1based.cmp(&b.obj_index_1based))
        });

        // By class: resolve names, sort shallow desc / class asc, keep top-N.
        let mut by_class: Vec<crate::report::TopArrayClassRow> = self
            .by_class
            .into_iter()
            .map(
                |(key, (objects, shallow))| crate::report::TopArrayClassRow {
                    array_class: name_of(key, p1),
                    objects,
                    shallow,
                },
            )
            .collect();
        by_class.sort_by(|a, b| {
            b.shallow
                .cmp(&a.shallow)
                .then_with(|| a.array_class.cmp(&b.array_class))
        });
        by_class.truncate(TOP_ARRAYS_N);

        crate::report::TopArrays {
            top_individual: individual,
            top_by_class: by_class,
        }
    }
}

/// Resolve a primitive-array class name from an element type code (the
/// `class_key` used by the primitive [`TopArrayAcc`]).
fn prim_array_name_of_key(elem_type: u64, _p1: &Pass1) -> String {
    crate::report::pretty_class_name(prim_array_class_name(elem_type as u8))
}

/// Resolve an object-array class name from its array-class object id (the
/// `class_key` used by the object [`TopArrayAcc`]). `class_map`/`strings` are
/// still alive at assembly time.
fn obj_array_name_of_key(array_class_id: u64, p1: &Pass1) -> String {
    p1.class_map
        .get(&array_class_id)
        .and_then(|ci| p1.strings.get(&ci.name_id))
        .map(|raw| crate::report::pretty_class_name(raw))
        .unwrap_or_else(|| format!("0x{array_class_id:x}"))
}

/// One holder→pointee edge collected under `--collections`: a non-null object
/// field of some instance. Names are INTERNED by key to keep each edge at 16
/// bytes. Joined post-scan against [`ContainerRecord`]s.
struct HolderEdge {
    pointee: u64,
    holder_class_key: u32,
    field_key: u32,
}

/// A container (collection/obj-array/prim-array) seen under `--collections`,
/// keyed by its address; carries its dense object index + element count + kind
/// (0=List,1=Map,2=Set,3=Deque,4=Queue,5=Tree,6=object array,7=primitive array)
/// + resolved class name.
struct ContainerRecord {
    container_idx: u32,
    elements: u64,
    kind: u8,
    container_class: String,
}

/// Join holder edges against container records: for each edge whose `pointee`
/// matches a container's address, emit one [`AttributionRaw`] (resolving the
/// interned holder-class/field keys to owned Strings). Retained size is NOT set
/// here — build_model fills it via `container_idx`.
fn join_attribution(
    edges: &[HolderEdge],
    container_records: &HashMap<u64, ContainerRecord>,
    holder_class_names: &[String],
    field_names: &[String],
) -> Vec<AttributionRaw> {
    let mut out: Vec<AttributionRaw> = Vec::new();
    for e in edges {
        if let Some(rec) = container_records.get(&e.pointee) {
            out.push(AttributionRaw {
                container_idx: rec.container_idx,
                holder_class: holder_class_names[e.holder_class_key as usize].clone(),
                field: field_names[e.field_key as usize].clone(),
                container_kind: rec.kind,
                container_class: rec.container_class.clone(),
                elements: rec.elements,
            });
        }
    }
    out
}

/// Enumerate every Object-type instance field of `class_id`, returning
/// `(field_name_id, byte_offset)` for each. The layout mirrors
/// [`build_field_plans`]/[`field_offset`]: walk the super-chain CHILD-FIRST (as
/// collected, NOT reversed — HPROF stores instance field VALUES subclass-first),
/// accumulating byte offsets where Object fields are `obj_ref_width` wide and
/// primitives use `t.byte_size()`. Used by the `--collections` holder-edge scan
/// (memoized per class) and unit-tested directly.
fn enumerate_object_fields(
    class_id: u64,
    class_map: &HashMap<u64, crate::pass1::ClassInfo>,
    obj_ref_width: usize,
) -> Vec<(u64, u32)> {
    // Collect the super-chain child-first.
    let mut chain: Vec<u64> = Vec::new();
    let mut cur = class_id;
    loop {
        match class_map.get(&cur) {
            None => break,
            Some(ci) => {
                chain.push(cur);
                if ci.super_id == 0 {
                    break;
                }
                cur = ci.super_id;
            }
        }
    }
    let mut out: Vec<(u64, u32)> = Vec::new();
    let mut byte_offset = 0usize;
    for &caddr in chain.iter() {
        let ci = match class_map.get(&caddr) {
            Some(c) => c,
            None => break,
        };
        for &(fname_id, t) in &ci.fields {
            if t == HprofType::Object {
                out.push((fname_id, byte_offset as u32));
                byte_offset += obj_ref_width;
            } else {
                byte_offset += t.byte_size();
            }
        }
    }
    out
}

/// Format a pretty class name (HPROF `/`-separated → `.`-separated).
fn pretty_name(class_id: u64, p1: &Pass1) -> String {
    p1.class_map
        .get(&class_id)
        .and_then(|ci| p1.strings.get(&ci.name_id))
        .map(|s| s.replace('/', "."))
        .unwrap_or_else(|| format!("0x{class_id:x}"))
}

/// Resolve the pretty class NAME of the object at dense index `i` in the id_map.
/// kind 2 objects are primitive arrays (class_ids holds the element type code);
/// kinds 0/1/3 index `class_addr_table` for the class-object address.
fn class_name_of_index(i: usize, p1: &Pass1) -> String {
    let kind = p1.kind.get(i).copied().unwrap_or(0);
    let raw = p1.class_ids.get(i).copied().unwrap_or(0);
    if kind == 2 {
        prim_array_class_name(raw as u8).to_string()
    } else {
        let class_addr = p1.class_addr_table.get(raw as usize).copied().unwrap_or(0);
        pretty_name(class_addr, p1)
    }
}

// ── Main entry point ─────────────────────────────────────────────────────────

/// Return type of [`build_field_decode_views`]: the collection & reference
/// views, the per-kind referent indices, the optional raw attribution records
/// (`Some` only under `--collections`), a truncation flag, and the sum of
/// `capacity` fields across all `java/nio/DirectByteBuffer` instances.
type FieldDecodeViews = (
    CollectionsAnalysis,
    ReferencesAnalysis,
    [Vec<u32>; 3],
    Option<Vec<AttributionRaw>>,
    bool,
    u64, // direct_byte_buffer_capacity_sum
);

/// Compute the collection/array/reference views in exactly THREE full-file
/// scans. Returns `(collections, references, reference_referent_idx)`; the
/// caller computes `only_weakly_retained` later from the referent indices +
/// `idom`. Must run while `p1.class_map` / `p1.strings` are still alive.
pub(crate) fn build_field_decode_views(
    path: &str,
    p1: &Pass1,
    shallow: &[u32],
    collect_attribution: bool,
    descs: &[CollDesc],
) -> std::io::Result<FieldDecodeViews> {
    let id_size = p1.id_size;
    // Instance-blob object references are id_size wide (compressed OOPs only
    // narrows object-ARRAY elements). Both instance fields and obj-array
    // elements here are id_size wide per the scanners.
    let obj_ref_width = id_size as usize;
    let class_map = &p1.class_map;
    let strings = &p1.strings;

    // Memoized per-class classification (bounded by #classes).
    let mut role_cache: HashMap<u64, ClassRole> = HashMap::new();

    // Collection views.
    let mut coll_size_acc = SizeHistAcc::default();
    let mut coll_fill = FillAcc::default();
    let mut map_collision = FillAcc::default();
    let mut coll_total: u64 = 0; // every collection with a size read
    let mut map_total: u64 = 0; // every map collection seen
    // Wanted backing arrays (array addr → its collection's size/is_map).
    let mut wanted_arrays: HashMap<u64, ArrayWant> = HashMap::new();

    // Reference views.
    let mut ref_instances = [0u64; 3];
    // kind → (class name → (objects, shallow)). Capped at REFERENT_HIST_CAP
    // distinct classes.
    let mut ref_hist: [HashMap<String, (u64, u64)>; 3] =
        [HashMap::new(), HashMap::new(), HashMap::new()];
    // kind → (objects, shallow) folded into the "<other>" row past the cap.
    let mut ref_hist_other = [(0u64, 0u64); 3];
    let mut referent_idx: [Vec<u32>; 3] = [Vec::new(), Vec::new(), Vec::new()];

    // Top arrays (individual + per-class), one per array category.
    let mut top_prim = TopArrayAcc::default();
    let mut top_obj = TopArrayAcc::default();

    // ── Attribution state (only allocated / touched under --collections) ──────
    let mut edges: Vec<HolderEdge> = Vec::new();
    let mut edges_truncated = false;
    // Interning: class_id → key (holder class names) and field name_id → key.
    let mut holder_class_names: Vec<String> = Vec::new();
    let mut holder_class_map: HashMap<u64, u32> = HashMap::new();
    let mut field_names: Vec<String> = Vec::new();
    let mut field_name_map: HashMap<u64, u32> = HashMap::new();
    // Memoized per-class object-field layout: class_id → [(field_name_key, off)].
    let mut obj_field_layout: HashMap<u64, Vec<(u32, u32)>> = HashMap::new();
    let mut container_records: HashMap<u64, ContainerRecord> = HashMap::new();
    let mut containers_truncated = false;

    // ── DirectByteBuffer capacity sum ────────────────────────────────────────
    // Resolve once before scan 1: find the class-object address for
    // java/nio/DirectByteBuffer and the byte-offset of the `capacity` field
    // declared on java/nio/Buffer. Both lookups use class_map/strings, which
    // are alive through the end of build_field_decode_views.
    let target_dbb_class = "java/nio/DirectByteBuffer";
    let dbb_class_addr_opt: Option<u64> = p1
        .class_map
        .iter()
        .find(|(_, ci)| {
            p1.strings
                .get(&ci.name_id)
                .map(|s| s.as_str())
                .unwrap_or("")
                == target_dbb_class
        })
        .map(|(addr, _)| *addr);
    let dbb_cap_off_opt: Option<u32> = dbb_class_addr_opt.and_then(|class_addr| {
        field_offset(
            class_addr,
            "capacity",
            "java/nio/Buffer",
            class_map,
            strings,
            obj_ref_width,
        )
        .map(|(off, _ty)| off)
    });
    let mut dbb_capacity_sum: u64 = 0;

    // Scan-2 (prim-array) + scan-3 (obj-array) accumulators, declared up here so
    // the fused single-pass scan below can populate them from three disjoint
    // closures. group (type_code, len, value) → (objects, shallow). Capped;
    // overflow folds to "other".
    let mut const_groups: HashMap<(u8, u64, i64), (u64, u64)> = HashMap::new();
    let mut const_other: (u64, u64) = (0, 0);
    let mut const_truncated = false;
    let mut array_fill = FillAcc::default();
    // Raw per-obj-array (addr, non_null, count) collected during the pass; the
    // `wanted_arrays` collection-fill/map-collision fold runs AFTER the pass, in
    // memory, because an OBJ_ARRAY_DUMP may precede its owning collection's
    // INSTANCE_DUMP (HPROF has no record-ordering guarantee), so `wanted_arrays`
    // is not fully populated until the single scan completes. Sums are
    // order-independent, so folding post-pass is byte-identical to the old
    // scan-1-then-scan-3 ordering.
    let mut obj_array_raw: Vec<(u64, u64, u64)> = Vec::new();

    // ── Single fused full-file scan (instances + prim arrays + obj arrays) ─────
    // Replaces three separate full-file scans with ONE pass. Each record kind
    // dispatches to its own closure; the closures capture disjoint mutable
    // state (the only cross-record dependency, `wanted_arrays`, is resolved in a
    // post-pass loop). Only one record's bytes are resident at a time, so peak
    // RSS is unchanged.
    scan_all_records(path, id_size, |rec| match rec {
        // ── on_instance: every INSTANCE_DUMP ──────────────────────────────────
        Record::Instance(addr, class_id, blob) => {
            // ── DirectByteBuffer capacity accumulation ────────────────────────
            // Check before the role dispatch so it runs even when the class is
            // otherwise Plain. Uses the pre-resolved class address and field offset.
            if let (Some(dbb_addr), Some(cap_off)) = (dbb_class_addr_opt, dbb_cap_off_opt) {
                if class_id == dbb_addr {
                    let o = cap_off as usize;
                    // `capacity` is declared as `int` on java/nio/Buffer.
                    if o + 4 <= blob.len() {
                        let v =
                            i32::from_be_bytes([blob[o], blob[o + 1], blob[o + 2], blob[o + 3]]);
                        dbb_capacity_sum += v.max(0) as u64;
                    }
                }
            }
            let _ = addr; // addr used above only; suppress unused warning if roles don't use it
            let role = role_cache
                .entry(class_id)
                .or_insert_with(|| classify(class_id, class_map, strings, obj_ref_width, descs))
                .clone();
            match role {
                ClassRole::Plain => {}
                ClassRole::Collection {
                    desc_idx,
                    size_off,
                    array_off,
                } => {
                    let is_map = descs[desc_idx].kind == CollKind::Map;
                    // Shallow size of THIS collection instance (precomputed vec).
                    let coll_shallow = p1
                        .id_map
                        .index_of(addr)
                        .map(|i| shallow[i] as u64)
                        .unwrap_or(0);
                    // Read size (if this collection exposes a plain size field).
                    let size = size_off.and_then(|(off, ty)| read_int_field(blob, off, ty));
                    if let Some(size) = size {
                        coll_size_acc.add(size, coll_shallow);
                        coll_total += 1;
                        if is_map {
                            map_total += 1;
                        }
                    }
                    // Attribution: record this collection as a container (kind
                    // = its CollKind discriminant, 0=List..5=Tree). elements =
                    // the read size (0 when the collection has no plain
                    // size field, e.g. ConcurrentHashMap — documented limitation).
                    if collect_attribution {
                        if let Some(cidx) = p1.id_map.index_of(addr) {
                            if container_records.len() < CONTAINER_CAP {
                                container_records.insert(
                                    addr,
                                    ContainerRecord {
                                        container_idx: cidx as u32,
                                        elements: size.unwrap_or(0),
                                        kind: descs[desc_idx].kind.discriminant(),
                                        container_class: pretty_name(class_id, p1),
                                    },
                                );
                            } else {
                                containers_truncated = true;
                            }
                        }
                    }
                    // Read backing-array address, defer fill ratio to scan 3.
                    if let Some((aoff, _)) = array_off {
                        let ao = aoff as usize;
                        if ao + obj_ref_width <= blob.len() {
                            let arr_addr = read_ref(&blob[ao..], obj_ref_width);
                            if arr_addr != 0 && wanted_arrays.len() < WANTED_CAP {
                                wanted_arrays.insert(
                                    arr_addr,
                                    ArrayWant {
                                        size: size.unwrap_or(0),
                                        is_map,
                                        coll_shallow,
                                    },
                                );
                            }
                        }
                    }
                }
                ClassRole::Reference {
                    kind_idx,
                    referent_off,
                } => {
                    ref_instances[kind_idx] += 1;
                    let (off, _ty) = referent_off;
                    let o = off as usize;
                    if o + obj_ref_width <= blob.len() {
                        let referent = read_ref(&blob[o..], obj_ref_width);
                        if referent != 0 {
                            if let Some(ridx) = p1.id_map.index_of(referent) {
                                let name = class_name_of_index(ridx, p1);
                                let sh = shallow[ridx] as u64;
                                let hist = &mut ref_hist[kind_idx];
                                if hist.contains_key(&name) || hist.len() < REFERENT_HIST_CAP {
                                    let e = hist.entry(name).or_insert((0, 0));
                                    e.0 += 1;
                                    e.1 += sh;
                                } else {
                                    ref_hist_other[kind_idx].0 += 1;
                                    ref_hist_other[kind_idx].1 += sh;
                                }
                                if referent_idx[kind_idx].len() < REFERENT_CAP {
                                    referent_idx[kind_idx].push(ridx as u32);
                                }
                            }
                        }
                    }
                }
            }

            // Attribution: record a holder edge for every non-null object field of
            // this instance. Kept in its OWN block (role match already released) so
            // the interner/layout borrows don't fight the existing borrows.
            if collect_attribution && edges.len() < FIELD_REF_CAP {
                // Build the class's object-field layout once (memoized). The
                // or_insert_with closure interns field names into
                // field_names/field_name_map — distinct maps from obj_field_layout,
                // so the borrows don't conflict.
                obj_field_layout.entry(class_id).or_insert_with(|| {
                    let raw = enumerate_object_fields(class_id, class_map, obj_ref_width);
                    let mut layout: Vec<(u32, u32)> = Vec::with_capacity(raw.len());
                    for (fname_id, off) in raw {
                        let field_key = *field_name_map.entry(fname_id).or_insert_with(|| {
                            let key = field_names.len() as u32;
                            let fname = strings
                                .get(&fname_id)
                                .map(|s| s.to_string())
                                .unwrap_or_default();
                            field_names.push(fname);
                            key
                        });
                        layout.push((field_key, off));
                    }
                    layout
                });
                // Intern the holder-class key once per instance (not per field).
                let holder_class_key = *holder_class_map.entry(class_id).or_insert_with(|| {
                    let key = holder_class_names.len() as u32;
                    holder_class_names.push(pretty_name(class_id, p1));
                    key
                });
                let layout = &obj_field_layout[&class_id];
                for &(field_key, offset) in layout {
                    if edges.len() >= FIELD_REF_CAP {
                        edges_truncated = true;
                        break;
                    }
                    let o = offset as usize;
                    if o + obj_ref_width <= blob.len() {
                        let pointee = read_ref(&blob[o..], obj_ref_width);
                        if pointee != 0 {
                            edges.push(HolderEdge {
                                pointee,
                                holder_class_key,
                                field_key,
                            });
                        }
                    }
                }
            }
        }
        // ── on_prim_array: every PRIM_ARRAY_DUMP ──────────────────────────────
        Record::PrimArray(addr, elem_type, count, bytes) => {
            // Top prim arrays: fold EVERY primitive array (not just constant ones).
            // Key on the element type code; the name resolves without `class_ids`.
            let (idx, sh) = match p1.id_map.index_of(addr) {
                Some(i) => (i as u32, shallow[i] as u64),
                None => (u32::MAX, 0),
            };
            if idx != u32::MAX {
                top_prim.add(idx, elem_type as u64, count, sh);
            }
            // Attribution: record this primitive array as a container (kind 7).
            if collect_attribution && idx != u32::MAX {
                if container_records.len() < CONTAINER_CAP {
                    container_records.insert(
                        addr,
                        ContainerRecord {
                            container_idx: idx,
                            elements: count,
                            kind: 7,
                            container_class: prim_array_class_name(elem_type).to_string(),
                        },
                    );
                } else {
                    containers_truncated = true;
                }
            }
            if count < 2 {
                return;
            }
            let esz = match HprofType::from_code(elem_type) {
                Some(t) => t.byte_size(),
                None => return,
            };
            if esz == 0 || bytes.len() < esz * 2 {
                return;
            }
            let first = &bytes[0..esz];
            // All elements equal?
            let all_equal = bytes
                .chunks_exact(esz)
                .take(count as usize)
                .all(|c| c == first);
            if !all_equal {
                return;
            }
            let value = decode_prim_value(elem_type, first);
            let key = (elem_type, count, value);
            if const_groups.contains_key(&key) || const_groups.len() < CONST_ARRAY_CAP {
                let e = const_groups.entry(key).or_insert((0, 0));
                e.0 += 1;
                e.1 += sh;
            } else {
                const_truncated = true;
                const_other.0 += 1;
                const_other.1 += sh;
            }
        }
        // ── on_obj_array: every OBJ_ARRAY_DUMP ────────────────────────────────
        Record::ObjArray(addr, array_class_id, count, elem_ref_bytes) => {
            if count == 0 {
                return;
            }
            // Non-null slots: count nonzero id_size-wide refs.
            let mut non_null: u64 = 0;
            for slot in 0..count as usize {
                let off = slot * obj_ref_width;
                if off + obj_ref_width > elem_ref_bytes.len() {
                    break;
                }
                if read_ref(&elem_ref_bytes[off..], obj_ref_width) != 0 {
                    non_null += 1;
                }
            }
            // #11 array fill ratio over ALL object arrays. Attribute THIS array's
            // own shallow size.
            let (arr_idx, arr_shallow) = match p1.id_map.index_of(addr) {
                Some(i) => (i as u32, shallow[i] as u64),
                None => (u32::MAX, 0),
            };
            array_fill.add(non_null, count, arr_shallow, 0);

            // Top object arrays: fold EVERY object array. The per-class key is the
            // array class id read from the record — class_ids has been freed by now,
            // so we resolve names later via obj_array_name_of_key.
            if arr_idx != u32::MAX {
                top_obj.add(arr_idx, array_class_id, count, arr_shallow);
            }

            // Attribution: record this object array as a container (kind 6).
            if collect_attribution && arr_idx != u32::MAX {
                if container_records.len() < CONTAINER_CAP {
                    container_records.insert(
                        addr,
                        ContainerRecord {
                            container_idx: arr_idx,
                            elements: count,
                            kind: 6,
                            container_class: pretty_name(array_class_id, p1),
                        },
                    );
                } else {
                    containers_truncated = true;
                }
            }

            // Defer the tracked-collection fold (#9 / #13) to a post-pass loop:
            // `wanted_arrays` may not yet contain this array's owning collection
            // (its INSTANCE_DUMP can appear later in the file). Collect the raw
            // per-array data now; fold after the single scan completes.
            obj_array_raw.push((addr, non_null, count));
        }
    })?;

    // ── Post-pass fold: tracked-collection fill (#9) + map-collision (#13) ─────
    // `wanted_arrays` is now fully populated (the single scan is done), so this
    // in-memory loop reproduces the old scan-3 `wanted_arrays.get(&addr)` fold
    // exactly. The accumulators are order-independent sums, so the result is
    // byte-identical to folding inline during a dedicated obj-array scan.
    let mut coll_fill_tracked: u64 = 0;
    let mut map_collision_tracked: u64 = 0;
    for (addr, non_null, count) in obj_array_raw.drain(..) {
        if let Some(want) = wanted_arrays.get(&addr) {
            // ConcurrentHashMap has no plain size: approximate size as non-null
            // slot count (want.size stays 0 in that case).
            let used = if want.size > 0 { want.size } else { non_null };
            // #9 collection fill ratio: used / capacity(=count). wasted = the
            // unused slots' worth of refs (capacity - used) clamped >=0. shallow
            // attributes the COLLECTION instance (backing-array bytes already
            // counted under array_fill).
            let wasted = count
                .saturating_sub(used.min(count))
                .saturating_mul(obj_ref_width as u64);
            coll_fill.add(used, count, want.coll_shallow, wasted);
            coll_fill_tracked += 1;
            if want.is_map {
                // #13 map-collision proxy = occupied slots / capacity. A high
                // occupancy vs. size disparity hints at collisions/chaining.
                map_collision.add(non_null, count, want.coll_shallow, 0);
                map_collision_tracked += 1;
            }
        }
    }

    // ── Attribution join: match each holder edge's pointee against a container
    // record, emitting one AttributionRaw per hit (resolving interned names to
    // owned Strings while class_map/strings are still alive). ─────────────────
    let attribution_raw = if collect_attribution {
        Some(join_attribution(
            &edges,
            &container_records,
            &holder_class_names,
            &field_names,
        ))
    } else {
        None
    };
    let attribution_truncated = edges_truncated || containers_truncated;

    // ── Assemble collection views ─────────────────────────────────────────────
    let collections = CollectionsAnalysis {
        collection_fill_ratio: CollectionFillRatio {
            tracked: coll_fill_tracked,
            total: coll_total,
            buckets: coll_fill.into_buckets(),
        },
        collections_by_size: coll_size_acc.into_by_size(),
        array_fill_ratio: ArrayFillRatio {
            tracked: array_fill.total_objects(),
            buckets: array_fill.into_buckets(),
        },
        map_collision_ratio: MapCollisionRatio {
            tracked: map_collision_tracked,
            total: map_total,
            buckets: map_collision.into_buckets(),
        },
        constant_primitive_arrays: assemble_const_arrays(
            const_groups,
            const_other,
            const_truncated,
        ),
        top_prim_arrays: top_prim.into_top_arrays(p1, prim_array_name_of_key),
        top_obj_arrays: top_obj.into_top_arrays(p1, obj_array_name_of_key),
    };

    // ── Assemble reference views ──────────────────────────────────────────────
    let references = ReferencesAnalysis {
        soft: assemble_ref_stats("Soft", ref_instances[0], &ref_hist[0], ref_hist_other[0]),
        weak: assemble_ref_stats("Weak", ref_instances[1], &ref_hist[1], ref_hist_other[1]),
        phantom: assemble_ref_stats("Phantom", ref_instances[2], &ref_hist[2], ref_hist_other[2]),
    };

    Ok((
        collections,
        references,
        referent_idx,
        attribution_raw,
        attribution_truncated,
        dbb_capacity_sum,
    ))
}

/// Size-histogram accumulator (power-of-two upper bounds + empty count).
#[derive(Default)]
struct SizeHistAcc {
    tracked: u64,
    empty: u64,
    /// upper_len → (objects, shallow).
    buckets: std::collections::BTreeMap<u64, (u64, u64)>,
}

impl SizeHistAcc {
    fn add(&mut self, size: u64, shallow: u64) {
        self.tracked += 1;
        if size == 0 {
            self.empty += 1;
            return;
        }
        let upper = size_hist_upper(size);
        let e = self.buckets.entry(upper).or_insert((0, 0));
        e.0 += 1;
        e.1 += shallow;
    }
    fn into_by_size(self) -> CollectionsBySize {
        let buckets = self
            .buckets
            .into_iter()
            .map(|(upper_len, (objects, shallow))| SizeHistogramBucket {
                upper_len,
                objects,
                shallow,
            })
            .collect();
        CollectionsBySize {
            tracked: self.tracked,
            empty_count: self.empty,
            buckets,
        }
    }
}

impl FillAcc {
    fn total_objects(&self) -> u64 {
        self.objects.iter().sum()
    }
}

/// Build the ConstantPrimitiveArrays view, sorting rows deterministically
/// (objects desc, then array_class asc, then length asc) and appending an
/// "other" fold row when the cap was hit.
fn assemble_const_arrays(
    groups: HashMap<(u8, u64, i64), (u64, u64)>,
    other: (u64, u64),
    truncated: bool,
) -> ConstantPrimitiveArrays {
    let mut rows: Vec<ConstantArrayRow> = groups
        .into_iter()
        .map(
            |((elem_type, length, value), (objects, shallow))| ConstantArrayRow {
                array_class: crate::report::pretty_class_name(prim_array_class_name(elem_type)),
                length,
                value,
                objects,
                shallow,
            },
        )
        .collect();
    rows.sort_by(|a, b| {
        b.objects
            .cmp(&a.objects)
            .then(a.array_class.cmp(&b.array_class))
            .then(a.length.cmp(&b.length))
            .then(a.value.cmp(&b.value))
    });
    if truncated && other.0 > 0 {
        rows.push(ConstantArrayRow {
            array_class: "<other>".to_string(),
            length: 0,
            value: 0,
            objects: other.0,
            shallow: other.1,
        });
    }
    ConstantPrimitiveArrays { rows, truncated }
}

/// Build one ReferenceStats, or None when the kind is absent. Rows sorted
/// objects desc then class asc; an "<other>" row folds classes beyond the cap.
fn assemble_ref_stats(
    kind: &str,
    instances: u64,
    hist: &HashMap<String, (u64, u64)>,
    other: (u64, u64),
) -> Option<ReferenceStats> {
    if instances == 0 {
        return None;
    }
    let mut rows: Vec<RefStatClassRow> = hist
        .iter()
        .map(|(name, &(objects, shallow))| RefStatClassRow {
            pretty_class: name.clone(),
            objects,
            shallow,
        })
        .collect();
    rows.sort_by(|a, b| {
        b.objects
            .cmp(&a.objects)
            .then(a.pretty_class.cmp(&b.pretty_class))
    });
    if other.0 > 0 {
        rows.push(RefStatClassRow {
            pretty_class: "<other>".to_string(),
            objects: other.0,
            shallow: other.1,
        });
    }
    Some(ReferenceStats {
        kind: kind.to_string(),
        reference_instances: instances,
        referent_histogram: rows,
        only_weakly_retained: Vec::new(),
    })
}

/// Read an integer instance field from a big-endian INSTANCE_DUMP blob at
/// `off`, interpreting per `ty`. Returns the value as u64 (sign not needed for
/// a collection size). None when the field type is non-integral or out of range.
fn read_int_field(blob: &[u8], off: u32, ty: HprofType) -> Option<u64> {
    let o = off as usize;
    match ty {
        HprofType::Int => {
            if o + 4 > blob.len() {
                return None;
            }
            let v = i32::from_be_bytes([blob[o], blob[o + 1], blob[o + 2], blob[o + 3]]);
            Some(v.max(0) as u64)
        }
        HprofType::Short => {
            if o + 2 > blob.len() {
                return None;
            }
            let v = i16::from_be_bytes([blob[o], blob[o + 1]]);
            Some(v.max(0) as u64)
        }
        HprofType::Long => {
            if o + 8 > blob.len() {
                return None;
            }
            let v = i64::from_be_bytes([
                blob[o],
                blob[o + 1],
                blob[o + 2],
                blob[o + 3],
                blob[o + 4],
                blob[o + 5],
                blob[o + 6],
                blob[o + 7],
            ]);
            Some(v.max(0) as u64)
        }
        _ => None,
    }
}

/// Decode a single primitive-array element (big-endian) to an i64 for the
/// constant-array row's `value`. Floating types are bit-cast into the i64 so a
/// constant fill is still recorded exactly.
fn decode_prim_value(elem_type: u8, bytes: &[u8]) -> i64 {
    match HprofType::from_code(elem_type) {
        Some(HprofType::Boolean) | Some(HprofType::Byte) => {
            if bytes.is_empty() {
                0
            } else {
                bytes[0] as i8 as i64
            }
        }
        Some(HprofType::Char) | Some(HprofType::Short) => {
            if bytes.len() < 2 {
                0
            } else {
                i16::from_be_bytes([bytes[0], bytes[1]]) as i64
            }
        }
        Some(HprofType::Int) | Some(HprofType::Float) => {
            if bytes.len() < 4 {
                0
            } else {
                i32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as i64
            }
        }
        Some(HprofType::Long) | Some(HprofType::Double) => {
            if bytes.len() < 8 {
                0
            } else {
                i64::from_be_bytes([
                    bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
                ])
            }
        }
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pass1::ClassInfo;

    fn strid(map: &mut HashMap<u64, String>, id: u64, name: &str) {
        map.insert(id, name.to_string());
    }

    /// Build a minimal class_map + strings for HashMap/ArrayList and a subclass
    /// of ArrayList, so offset resolution can be tested hermetically.
    fn fixture() -> (HashMap<u64, ClassInfo>, HashMap<u64, String>) {
        let mut strings: HashMap<u64, String> = HashMap::new();
        // string ids
        strid(&mut strings, 1, "java/util/HashMap");
        strid(&mut strings, 2, "java/util/ArrayList");
        strid(&mut strings, 3, "size");
        strid(&mut strings, 4, "table");
        strid(&mut strings, 5, "elementData");
        strid(&mut strings, 6, "MyList"); // subclass of ArrayList
        strid(&mut strings, 7, "extra");
        strid(&mut strings, 8, "java/lang/Object");

        let mut class_map: HashMap<u64, ClassInfo> = HashMap::new();
        // java/lang/Object @ 0x10 (root; super_id 0, no fields)
        class_map.insert(
            0x10,
            ClassInfo {
                name_id: 8,
                super_id: 0,
                ..Default::default()
            },
        );
        // java/util/HashMap @ 0x20: fields size(Int), table(Object).
        class_map.insert(
            0x20,
            ClassInfo {
                name_id: 1,
                super_id: 0x10,
                fields: vec![(3, HprofType::Int), (4, HprofType::Object)],
                ..Default::default()
            },
        );
        // java/util/ArrayList @ 0x30: fields size(Int), elementData(Object).
        class_map.insert(
            0x30,
            ClassInfo {
                name_id: 2,
                super_id: 0x10,
                fields: vec![(3, HprofType::Int), (5, HprofType::Object)],
                ..Default::default()
            },
        );
        // MyList @ 0x40 extends ArrayList: adds extra(Int) declared child-first.
        class_map.insert(
            0x40,
            ClassInfo {
                name_id: 6,
                super_id: 0x30,
                fields: vec![(7, HprofType::Int)],
                ..Default::default()
            },
        );
        (class_map, strings)
    }

    #[test]
    fn resolves_hashmap_offsets() {
        let (class_map, strings) = fixture();
        let descs = builtin_coll_descs();
        let role = classify(0x20, &class_map, &strings, 8, &descs);
        match role {
            ClassRole::Collection {
                size_off,
                array_off,
                desc_idx,
            } => {
                assert_eq!(descs[desc_idx].class_name, "java/util/HashMap");
                // size Int at offset 0; table Object at offset 4.
                assert_eq!(size_off, Some((0, HprofType::Int)));
                assert_eq!(array_off, Some((4, HprofType::Object)));
            }
            _ => panic!("HashMap should classify as Collection"),
        }
    }

    #[test]
    fn resolves_arraylist_offsets() {
        let (class_map, strings) = fixture();
        let descs = builtin_coll_descs();
        let role = classify(0x30, &class_map, &strings, 8, &descs);
        match role {
            ClassRole::Collection {
                size_off,
                array_off,
                desc_idx,
            } => {
                assert_eq!(descs[desc_idx].class_name, "java/util/ArrayList");
                assert_eq!(size_off, Some((0, HprofType::Int)));
                assert_eq!(array_off, Some((4, HprofType::Object)));
            }
            _ => panic!("ArrayList should classify as Collection"),
        }
    }

    #[test]
    fn subclass_resolves_via_super_chain() {
        let (class_map, strings) = fixture();
        let descs = builtin_coll_descs();
        // MyList (0x40) is not itself in descs, but its super ArrayList is.
        let role = classify(0x40, &class_map, &strings, 8, &descs);
        match role {
            ClassRole::Collection {
                size_off,
                array_off,
                desc_idx,
            } => {
                assert_eq!(descs[desc_idx].class_name, "java/util/ArrayList");
                assert_eq!(descs[desc_idx].kind, CollKind::List);
                // MyList lays out its own `extra`(Int, off 0) first, then the
                // inherited ArrayList fields: size(Int) at 4, elementData at 8.
                assert_eq!(size_off, Some((4, HprofType::Int)));
                assert_eq!(array_off, Some((8, HprofType::Object)));
            }
            _ => panic!("MyList should classify as Collection via ArrayList"),
        }
    }

    #[test]
    fn bucket_quantization() {
        // 0/10 → ratio 0 → bucket 0.
        assert_eq!(ratio_bucket_index(0, 10), 0);
        // 5/10 → 5000 bp → band (4000,5000] → bucket 4.
        assert_eq!(ratio_bucket_index(5, 10), 4);
        // 10/10 → 10000 bp → last bucket.
        assert_eq!(ratio_bucket_index(10, 10), RATIO_BOUNDS.len() - 1);
        // 1/10 → 1000 bp → band (0,1000] → bucket 0.
        assert_eq!(ratio_bucket_index(1, 10), 0);
        // 1.5/10 → 1500 bp → band (1000,2000] → bucket 1.
        assert_eq!(ratio_bucket_index(3, 20), 1);
        // capacity 0 → bucket 0 (defensive).
        assert_eq!(ratio_bucket_index(5, 0), 0);
        // >100% clamps to last bucket.
        assert_eq!(ratio_bucket_index(15, 10), RATIO_BOUNDS.len() - 1);
    }

    #[test]
    fn size_hist_upper_bounds() {
        assert_eq!(size_hist_upper(0), 1);
        assert_eq!(size_hist_upper(1), 1);
        assert_eq!(size_hist_upper(2), 2);
        assert_eq!(size_hist_upper(3), 4);
        assert_eq!(size_hist_upper(8), 8);
        assert_eq!(size_hist_upper(9), 16);
    }

    #[test]
    fn new_descriptors_match_by_name() {
        // builtin_coll_descs() carries the Eclipse/Trove rows with the expected kinds.
        let descs = builtin_coll_descs();
        for (name, kind) in [
            (
                "org/eclipse/collections/impl/list/mutable/FastList",
                CollKind::List,
            ),
            (
                "org/eclipse/collections/impl/map/mutable/UnifiedMap",
                CollKind::Map,
            ),
            (
                "org/eclipse/collections/impl/set/mutable/UnifiedSet",
                CollKind::Set,
            ),
            ("gnu/trove/map/hash/THashMap", CollKind::Map),
            ("gnu/trove/set/hash/THashSet", CollKind::Set),
            ("gnu/trove/map/hash/TIntObjectHashMap", CollKind::Map),
        ] {
            let idx = descs
                .iter()
                .position(|d| d.class_name == name)
                .unwrap_or_else(|| panic!("{name} missing from builtin_coll_descs()"));
            assert_eq!(descs[idx].kind, kind, "{name} kind");
        }
    }

    #[test]
    fn top_array_heap_keeps_largest_by_shallow() {
        let mut acc = TopArrayAcc::default();
        // Feed more than TOP_ARRAYS_N candidates with increasing shallow; the
        // heap must retain only the TOP_ARRAYS_N largest.
        for i in 0..(TOP_ARRAYS_N as u32 + 5) {
            acc.add(i, 0, i as u64, (i as u64) * 100);
        }
        assert_eq!(acc.heap.len(), TOP_ARRAYS_N);
        // The retained candidates are the TOP_ARRAYS_N largest shallows.
        let mut shallows: Vec<u64> = acc.heap.iter().map(|r| r.0.shallow).collect();
        shallows.sort_unstable();
        let smallest_kept = shallows[0];
        // Anything with shallow < smallest_kept was evicted; the top N are 5..=14.
        assert_eq!(smallest_kept, 5 * 100);
        // by_class aggregated ALL candidates (not just the kept ones).
        let (objects, _) = acc.by_class[&0];
        assert_eq!(objects, TOP_ARRAYS_N as u64 + 5);
    }

    /// Resolve field name_id → name via the fixture strings for readable asserts.
    fn resolve_layout(
        class_id: u64,
        class_map: &HashMap<u64, ClassInfo>,
        strings: &HashMap<u64, String>,
    ) -> Vec<(String, u32)> {
        enumerate_object_fields(class_id, class_map, 8)
            .into_iter()
            .map(|(name_id, off)| (strings.get(&name_id).cloned().unwrap_or_default(), off))
            .collect()
    }

    #[test]
    fn enumerate_object_fields_hashmap() {
        let (class_map, strings) = fixture();
        // HashMap@0x20: size(Int)@0, table(Object)@4 → only the Object field.
        let layout = resolve_layout(0x20, &class_map, &strings);
        assert_eq!(layout, vec![("table".to_string(), 4)]);
    }

    #[test]
    fn enumerate_object_fields_subclass_super_chain() {
        let (class_map, strings) = fixture();
        // MyList@0x40 extends ArrayList: own extra(Int)@0, then inherited
        // size(Int)@4, elementData(Object)@8. Only elementData is an Object field.
        let layout = resolve_layout(0x40, &class_map, &strings);
        assert_eq!(layout, vec![("elementData".to_string(), 8)]);
    }

    #[test]
    fn join_matches_edge_to_container() {
        // One container at address 0x1000 (dense idx 7, kind 1, "java.util.X",
        // 42 elements) and two edges: one hits it, one points elsewhere.
        let holder_class_names = vec!["com.example.Holder".to_string()];
        let field_names = vec!["items".to_string()];
        let edges = vec![
            HolderEdge {
                pointee: 0x1000,
                holder_class_key: 0,
                field_key: 0,
            },
            HolderEdge {
                pointee: 0x2000, // no matching container
                holder_class_key: 0,
                field_key: 0,
            },
        ];
        let mut container_records: HashMap<u64, ContainerRecord> = HashMap::new();
        container_records.insert(
            0x1000,
            ContainerRecord {
                container_idx: 7,
                elements: 42,
                kind: 1, // round-trip value only; 1 now denotes Map
                container_class: "java.util.X".to_string(),
            },
        );
        let out = join_attribution(
            &edges,
            &container_records,
            &holder_class_names,
            &field_names,
        );
        assert_eq!(out.len(), 1);
        let row = &out[0];
        assert_eq!(row.container_idx, 7);
        assert_eq!(row.holder_class, "com.example.Holder");
        assert_eq!(row.field, "items");
        assert_eq!(row.container_kind, 1);
        assert_eq!(row.container_class, "java.util.X");
        assert_eq!(row.elements, 42);
    }
}

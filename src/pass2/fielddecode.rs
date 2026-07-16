//! Always-on field-decode deep-scan: computes five collection/array views
//! (collection fill ratio, collections-by-size histogram, object-array fill
//! ratio, map-collision proxy, constant primitive arrays) and three reference
//! views (soft/weak/phantom referent statistics) in exactly THREE shared
//! full-file scans.
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
    field_offset, prim_array_class_name, read_ref, scan_all_instances, scan_all_obj_arrays,
    scan_all_prim_arrays,
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

// ── Collection descriptor table ──────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum CollKind {
    List,
    Map,
    Set,
    #[allow(dead_code)]
    Deque,
    #[allow(dead_code)]
    Queue,
    #[allow(dead_code)]
    Tree,
}

struct CollDesc {
    class_name: &'static str,
    /// (field_name, declaring_owner_class) for the element-count field.
    size_field: Option<(&'static str, &'static str)>,
    /// (field_name, declaring_owner_class) for the backing object-array field.
    array_field: Option<(&'static str, &'static str)>,
    /// (field_name, declaring_owner_class) for a nested delegate collection
    /// (e.g. HashSet.map -> a HashMap); resolved as an object reference and
    /// currently used only for classification (Set/Tree wrappers).
    #[allow(dead_code)]
    nested_map_field: Option<(&'static str, &'static str)>,
    kind: CollKind,
}

/// HPROF class names use `/` separators (e.g. `java/util/HashMap`), so the
/// descriptor owner_class names below match `field_offset`'s expectation.
static COLL_DESCS: &[CollDesc] = &[
    CollDesc {
        class_name: "java/util/HashMap",
        size_field: Some(("size", "java/util/HashMap")),
        array_field: Some(("table", "java/util/HashMap")),
        nested_map_field: None,
        kind: CollKind::Map,
    },
    CollDesc {
        class_name: "java/util/LinkedHashMap",
        // size/table declared on java.util.HashMap (LinkedHashMap extends it).
        size_field: Some(("size", "java/util/HashMap")),
        array_field: Some(("table", "java/util/HashMap")),
        nested_map_field: None,
        kind: CollKind::Map,
    },
    CollDesc {
        class_name: "java/util/Hashtable",
        size_field: Some(("count", "java/util/Hashtable")),
        array_field: Some(("table", "java/util/Hashtable")),
        nested_map_field: None,
        kind: CollKind::Map,
    },
    CollDesc {
        class_name: "java/util/concurrent/ConcurrentHashMap",
        // size is not a plain field; approximate from non-null slots in scan 3.
        size_field: None,
        array_field: Some(("table", "java/util/concurrent/ConcurrentHashMap")),
        nested_map_field: None,
        kind: CollKind::Map,
    },
    CollDesc {
        class_name: "java/util/TreeMap",
        size_field: Some(("size", "java/util/TreeMap")),
        array_field: None,
        nested_map_field: None,
        kind: CollKind::Tree,
    },
    CollDesc {
        class_name: "java/util/ArrayList",
        size_field: Some(("size", "java/util/ArrayList")),
        array_field: Some(("elementData", "java/util/ArrayList")),
        nested_map_field: None,
        kind: CollKind::List,
    },
    CollDesc {
        class_name: "java/util/Vector",
        size_field: Some(("elementCount", "java/util/Vector")),
        array_field: Some(("elementData", "java/util/Vector")),
        nested_map_field: None,
        kind: CollKind::List,
    },
    CollDesc {
        class_name: "java/util/LinkedList",
        size_field: Some(("size", "java/util/LinkedList")),
        array_field: None,
        nested_map_field: None,
        kind: CollKind::List,
    },
    CollDesc {
        class_name: "java/util/ArrayDeque",
        size_field: None,
        array_field: Some(("elements", "java/util/ArrayDeque")),
        nested_map_field: None,
        kind: CollKind::Deque,
    },
    CollDesc {
        class_name: "java/util/HashSet",
        size_field: None,
        array_field: None,
        nested_map_field: Some(("map", "java/util/HashSet")),
        kind: CollKind::Set,
    },
    CollDesc {
        class_name: "java/util/TreeSet",
        size_field: None,
        array_field: None,
        nested_map_field: Some(("m", "java/util/TreeSet")),
        kind: CollKind::Set,
    },
    CollDesc {
        class_name: "scala/collection/mutable/HashMap",
        size_field: Some(("contentSize", "scala/collection/mutable/HashMap")),
        array_field: Some(("table", "scala/collection/mutable/HashMap")),
        nested_map_field: None,
        kind: CollKind::Map,
    },
    CollDesc {
        class_name: "scala/collection/mutable/ArrayBuffer",
        size_field: Some(("size0", "scala/collection/mutable/ArrayBuffer")),
        array_field: Some(("array", "scala/collection/mutable/ArrayBuffer")),
        nested_map_field: None,
        kind: CollKind::List,
    },
];

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

/// Walk `class_id`'s super-chain (child-first) and return the index of the
/// FIRST COLL_DESC whose class_name matches a class in the chain, or None.
fn match_coll_desc(
    class_id: u64,
    class_map: &HashMap<u64, crate::pass1::ClassInfo>,
    strings: &HashMap<u64, String>,
) -> Option<usize> {
    let mut cur = class_id;
    loop {
        let ci = class_map.get(&cur)?;
        let cname = strings.get(&ci.name_id).map(|s| s.as_str()).unwrap_or("");
        if let Some(idx) = COLL_DESCS.iter().position(|d| d.class_name == cname) {
            return Some(idx);
        }
        if ci.super_id == 0 {
            return None;
        }
        cur = ci.super_id;
    }
}

/// Walk `class_id`'s super-chain (child-first) and return the ref-kind index
/// (0 soft / 1 weak / 2 phantom) of the FIRST matching REF_CLASSES entry, else
/// None.
fn match_ref_kind(
    class_id: u64,
    class_map: &HashMap<u64, crate::pass1::ClassInfo>,
    strings: &HashMap<u64, String>,
) -> Option<usize> {
    let mut cur = class_id;
    loop {
        let ci = class_map.get(&cur)?;
        let cname = strings.get(&ci.name_id).map(|s| s.as_str()).unwrap_or("");
        if let Some(idx) = REF_CLASSES.iter().position(|&r| r == cname) {
            return Some(idx);
        }
        if ci.super_id == 0 {
            return None;
        }
        cur = ci.super_id;
    }
}

/// Classify a class-object address once, resolving the field offsets it needs.
fn classify(
    class_id: u64,
    class_map: &HashMap<u64, crate::pass1::ClassInfo>,
    strings: &HashMap<u64, String>,
    obj_ref_width: usize,
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
    if let Some(desc_idx) = match_coll_desc(class_id, class_map, strings) {
        let desc = &COLL_DESCS[desc_idx];
        let size_off = desc.size_field.and_then(|(name, owner)| {
            field_offset(class_id, name, owner, class_map, strings, obj_ref_width)
        });
        let array_off = desc.array_field.and_then(|(name, owner)| {
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

/// Compute the collection/array/reference views in exactly THREE full-file
/// scans. Returns `(collections, references, reference_referent_idx)`; the
/// caller computes `only_weakly_retained` later from the referent indices +
/// `idom`. Must run while `p1.class_map` / `p1.strings` are still alive.
pub(crate) fn build_field_decode_views(
    path: &str,
    p1: &Pass1,
    shallow: &[u32],
    _ref_size: usize,
) -> std::io::Result<(CollectionsAnalysis, ReferencesAnalysis, [Vec<u32>; 3])> {
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

    // ── Scan 1: every INSTANCE_DUMP ──────────────────────────────────────────
    scan_all_instances(path, id_size, |addr, class_id, blob| {
        let role = role_cache
            .entry(class_id)
            .or_insert_with(|| classify(class_id, class_map, strings, obj_ref_width))
            .clone();
        match role {
            ClassRole::Plain => {}
            ClassRole::Collection {
                desc_idx,
                size_off,
                array_off,
            } => {
                let is_map = COLL_DESCS[desc_idx].kind == CollKind::Map;
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
    })?;

    // ── Scan 2: every PRIM_ARRAY_DUMP — constant-array detection ──────────────
    // group (type_code, len, value) → objects. Capped; overflow folds to "other".
    let mut const_groups: HashMap<(u8, u64, i64), u64> = HashMap::new();
    let mut const_other_objects: u64 = 0;
    let mut const_truncated = false;
    scan_all_prim_arrays(path, id_size, |_addr, elem_type, count, bytes| {
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
            *const_groups.entry(key).or_insert(0) += 1;
        } else {
            const_truncated = true;
            const_other_objects += 1;
        }
    })?;

    // ── Scan 3: every OBJ_ARRAY_DUMP — array fill ratio + collection folds ────
    let mut array_fill = FillAcc::default();
    let mut coll_fill_tracked: u64 = 0;
    let mut map_collision_tracked: u64 = 0;
    scan_all_obj_arrays(path, id_size, |addr, count, elem_ref_bytes| {
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
        let arr_shallow = p1
            .id_map
            .index_of(addr)
            .map(|i| shallow[i] as u64)
            .unwrap_or(0);
        array_fill.add(non_null, count, arr_shallow, 0);

        // If this is a tracked collection backing array, fold #9 / #13.
        if let Some(want) = wanted_arrays.get(&addr) {
            // ConcurrentHashMap has no plain size: approximate size as non-null
            // slot count (want.size stays 0 in that case).
            let used = if want.size > 0 { want.size } else { non_null };
            // #9 collection fill ratio: used / capacity(=count). wasted = the
            // unused slots' worth of refs (capacity - used) clamped >=0. shallow
            // attributes the COLLECTION instance (backing-array bytes already
            // counted under array_fill).
            let wasted = count.saturating_sub(used.min(count)) * obj_ref_width as u64;
            coll_fill.add(used, count, want.coll_shallow, wasted);
            coll_fill_tracked += 1;
            if want.is_map {
                // #13 map-collision proxy = occupied slots / capacity. A high
                // occupancy vs. size disparity hints at collisions/chaining.
                map_collision.add(non_null, count, want.coll_shallow, 0);
                map_collision_tracked += 1;
            }
        }
    })?;

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
            const_other_objects,
            const_truncated,
        ),
    };

    // ── Assemble reference views ──────────────────────────────────────────────
    let references = ReferencesAnalysis {
        soft: assemble_ref_stats("Soft", ref_instances[0], &ref_hist[0], ref_hist_other[0]),
        weak: assemble_ref_stats("Weak", ref_instances[1], &ref_hist[1], ref_hist_other[1]),
        phantom: assemble_ref_stats("Phantom", ref_instances[2], &ref_hist[2], ref_hist_other[2]),
    };

    Ok((collections, references, referent_idx))
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
    groups: HashMap<(u8, u64, i64), u64>,
    other_objects: u64,
    truncated: bool,
) -> ConstantPrimitiveArrays {
    let mut rows: Vec<ConstantArrayRow> = groups
        .into_iter()
        .map(|((elem_type, length, value), objects)| ConstantArrayRow {
            array_class: prim_array_class_name(elem_type).to_string(),
            length,
            value,
            objects,
            shallow: 0,
        })
        .collect();
    rows.sort_by(|a, b| {
        b.objects
            .cmp(&a.objects)
            .then(a.array_class.cmp(&b.array_class))
            .then(a.length.cmp(&b.length))
            .then(a.value.cmp(&b.value))
    });
    if truncated && other_objects > 0 {
        rows.push(ConstantArrayRow {
            array_class: "<other>".to_string(),
            length: 0,
            value: 0,
            objects: other_objects,
            shallow: 0,
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
        let role = classify(0x20, &class_map, &strings, 8);
        match role {
            ClassRole::Collection {
                size_off,
                array_off,
                desc_idx,
            } => {
                assert_eq!(COLL_DESCS[desc_idx].class_name, "java/util/HashMap");
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
        let role = classify(0x30, &class_map, &strings, 8);
        match role {
            ClassRole::Collection {
                size_off,
                array_off,
                desc_idx,
            } => {
                assert_eq!(COLL_DESCS[desc_idx].class_name, "java/util/ArrayList");
                assert_eq!(size_off, Some((0, HprofType::Int)));
                assert_eq!(array_off, Some((4, HprofType::Object)));
            }
            _ => panic!("ArrayList should classify as Collection"),
        }
    }

    #[test]
    fn subclass_resolves_via_super_chain() {
        let (class_map, strings) = fixture();
        // MyList (0x40) is not itself in COLL_DESCS, but its super ArrayList is.
        let role = classify(0x40, &class_map, &strings, 8);
        match role {
            ClassRole::Collection {
                size_off,
                array_off,
                desc_idx,
            } => {
                assert_eq!(COLL_DESCS[desc_idx].class_name, "java/util/ArrayList");
                assert_eq!(COLL_DESCS[desc_idx].kind, CollKind::List);
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
}

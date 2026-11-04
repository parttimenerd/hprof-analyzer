//! Shared collection-descriptor table used by both the field-decode pass
//! (`pass2/fielddecode.rs`) and the OQL query engine.

use std::collections::HashMap;

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
    pub(crate) fn discriminant(self) -> u8 {
        match self {
            CollKind::List => 0,
            CollKind::Map => 1,
            CollKind::Set => 2,
            CollKind::Deque => 3,
            CollKind::Queue => 4,
            CollKind::Tree => 5,
        }
    }

    /// Lowercase per-kind label, matching `kind_label` in report/build.rs
    /// (0=list..5=tree). Used for the per-kind collection summary rows.
    pub(crate) fn label(self) -> &'static str {
        match self {
            CollKind::List => "list",
            CollKind::Map => "map",
            CollKind::Set => "set",
            CollKind::Deque => "deque",
            CollKind::Queue => "queue",
            CollKind::Tree => "tree",
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

/// Walk `class_id`'s super-chain (child-first) and return the index of the
/// FIRST CollDesc in `descs` whose class_name matches a class in the chain, or None.
pub(crate) fn match_coll_desc(
    class_id: u64,
    class_map: &HashMap<u64, crate::pass1::ClassInfo>,
    strings: &HashMap<u64, String>,
    descs: &[CollDesc],
) -> Option<usize> {
    let mut cur = class_id;
    loop {
        let ci = class_map.get(&cur)?;
        let cname = strings.get(&ci.name_id).map(|s| s.as_str()).unwrap_or("");
        if let Some(idx) = descs.iter().position(|d| d.class_name == cname) {
            return Some(idx);
        }
        if ci.super_id == 0 {
            return None;
        }
        cur = ci.super_id;
    }
}

/// Return the index in `descs` whose `class_name` exactly matches `slash_name`,
/// or `None` if not found. This is a direct name lookup (no super-chain walk).
#[allow(dead_code)]
pub(crate) fn find_desc_by_name(slash_name: &str, descs: &[CollDesc]) -> Option<usize> {
    descs.iter().position(|d| d.class_name == slash_name)
}

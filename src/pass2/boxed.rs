//! Boxed-number holder ranking (`--collections` opt-in).
//!
//! Both phases are folded into the main pass2 scans:
//!
//! 1. Addresses of all live boxed-type instances (java.lang.Integer, Long,
//!    Double, …) are captured during the 2a scan ([`boxed_class_addrs`] is the
//!    shared predicate).
//! 2. Per-class ref counts to those addresses are accumulated during the 2b
//!    forward-CSR fill, then turned into the top-20 holder list here by
//!    [`build_boxed_holders_from_counts`] (sorted by `boxed_refs` descending).

use std::collections::{HashMap, HashSet};

const BOXED_HOLDER_TOP_N: usize = 20;

const BOXED_TYPES: &[&str] = &[
    "java/lang/Boolean",
    "java/lang/Byte",
    "java/lang/Character",
    "java/lang/Short",
    "java/lang/Integer",
    "java/lang/Long",
    "java/lang/Float",
    "java/lang/Double",
    "java/lang/BigInteger",
    "java/lang/BigDecimal",
];

/// Class addresses of boxed-number types ([`BOXED_TYPES`]). Shared with the 2a
/// scan so it can capture boxed instance addresses using the identical predicate.
pub(crate) fn boxed_class_addrs(
    class_map: &HashMap<u64, crate::pass1::ClassInfo>,
    strings: &HashMap<u64, String>,
) -> HashSet<u64> {
    class_map
        .iter()
        .filter(|(_, ci)| {
            strings
                .get(&ci.name_id)
                .map(|n| BOXED_TYPES.contains(&n.as_str()))
                .unwrap_or(false)
        })
        .map(|(&addr, _)| addr)
        .collect()
}

/// Build the sorted, truncated top-N holder list from per-class boxed-ref counts.
///
/// `name_of` resolves a holder class address to its dotted class name. Fed by the
/// 2b-scan fold in `mod.rs`, which accumulates the `class_id -> count` map during
/// the forward-CSR fill and resolves names from a snapshot captured before
/// class_map/strings are freed.
pub(crate) fn build_boxed_holders_from_counts(
    class_counter: HashMap<u64, u64>,
    name_of: impl Fn(u64) -> String,
) -> Vec<crate::report::BoxedNumberHolder> {
    let mut holders: Vec<crate::report::BoxedNumberHolder> = class_counter
        .into_iter()
        .map(
            |(class_addr, boxed_refs)| crate::report::BoxedNumberHolder {
                class_name: name_of(class_addr),
                boxed_refs,
            },
        )
        .collect();
    holders.sort_unstable_by(|a, b| {
        b.boxed_refs
            .cmp(&a.boxed_refs)
            .then(a.class_name.cmp(&b.class_name))
    });
    holders.truncate(BOXED_HOLDER_TOP_N);
    holders
}

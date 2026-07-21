//! Boxed-number holder ranking (`--collections` opt-in).
//!
//! Two-pass scan:
//!   1. Collect addresses of all live boxed-type instances
//!      (java.lang.Integer, Long, Double, …).
//!   2. FieldPlan scan to count how many object-reference fields
//!      of each class point at those addresses.
//! Returns the top-20 holder classes sorted by `boxed_refs` descending.

use std::collections::{HashMap, HashSet};
use std::io;

use super::strings::scan_all_instances;
use super::{build_field_plans, read_ref};
use crate::pass1::Pass1;

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

/// Compute top holder classes for boxed-number objects.
/// Performs two full-file scans (collect addrs, then count refs).
pub(crate) fn compute_boxed_holders(
    path: &str,
    p1: &Pass1,
    id_size: u8,
) -> io::Result<Vec<crate::report::BoxedNumberHolder>> {
    let class_map = &p1.class_map;
    let strings = &p1.strings;

    // Build a set of class addresses that correspond to boxed types.
    let boxed_class_addrs: HashSet<u64> = class_map
        .iter()
        .filter(|(_, ci)| {
            strings
                .get(&ci.name_id)
                .map(|n| BOXED_TYPES.contains(&n.as_str()))
                .unwrap_or(false)
        })
        .map(|(&addr, _)| addr)
        .collect();

    if boxed_class_addrs.is_empty() {
        return Ok(Vec::new());
    }

    // Pass 1: collect addresses of all live boxed-type instances.
    let mut boxed_addrs: HashSet<u64> = HashSet::new();
    scan_all_instances(path, id_size, |obj_addr, class_id, _blob| {
        if boxed_class_addrs.contains(&class_id) {
            boxed_addrs.insert(obj_addr);
        }
    })?;

    if boxed_addrs.is_empty() {
        return Ok(Vec::new());
    }

    // Pass 2: FieldPlan scan — count refs to boxed addrs per class.
    let obj_ref_width = id_size as usize;
    let field_plans = build_field_plans(class_map, strings, id_size as usize);
    let mut class_counter: HashMap<u64, u64> = HashMap::new();

    scan_all_instances(path, id_size, |_obj_addr, class_id, blob| {
        let Some(plan) = field_plans.get(&class_id) else {
            return;
        };
        let mut hits: u64 = 0;
        for &(offset, _excluded) in plan {
            let off = offset as usize;
            if off + obj_ref_width > blob.len() {
                continue;
            }
            let r = read_ref(&blob[off..], obj_ref_width);
            if r != 0 && boxed_addrs.contains(&r) {
                hits += 1;
            }
        }
        if hits > 0 {
            *class_counter.entry(class_id).or_insert(0) += hits;
        }
    })?;

    let mut holders: Vec<crate::report::BoxedNumberHolder> = class_counter
        .into_iter()
        .map(|(class_addr, boxed_refs)| {
            let class_name = class_map
                .get(&class_addr)
                .and_then(|ci| strings.get(&ci.name_id))
                .map(|s| s.replace('/', "."))
                .unwrap_or_else(|| format!("0x{class_addr:x}"));
            crate::report::BoxedNumberHolder { class_name, boxed_refs }
        })
        .collect();
    holders.sort_unstable_by(|a, b| {
        b.boxed_refs
            .cmp(&a.boxed_refs)
            .then(a.class_name.cmp(&b.class_name))
    });
    holders.truncate(BOXED_HOLDER_TOP_N);
    Ok(holders)
}

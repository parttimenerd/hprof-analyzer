use crate::pass2::model::Graph;
use crate::report::FrameworkAnalysis;

/// Sentinels: (jvm_class_name_with_slashes, framework_label)
static SENTINELS: &[(&str, &str)] = &[
    ("org/hibernate/internal/SessionImpl", "Hibernate"),
    (
        "org/springframework/context/support/AbstractApplicationContext",
        "Spring",
    ),
    (
        "java/util/concurrent/ThreadPoolExecutor",
        "ThreadPoolExecutor",
    ),
    ("io/netty/buffer/AbstractReferenceCountedByteBuf", "Netty"),
    ("com/zaxxer/hikari/pool/HikariPool", "HikariCP"),
];

/// Requires `g.retained` to be populated (call after the retained-size stage).
pub fn scan_frameworks(g: &Graph) -> Vec<FrameworkAnalysis> {
    // Build a map from class index → sentinel slot so we do a single O(n) pass
    // over g.class_idx instead of one pass per sentinel.
    let mut ci_to_slot: std::collections::HashMap<u32, usize> = std::collections::HashMap::new();
    for (slot, &(sentinel, _)) in SENTINELS.iter().enumerate() {
        for (i, name) in g.class_names.iter().enumerate() {
            if name.as_str() == sentinel {
                ci_to_slot.insert(i as u32, slot);
            }
        }
    }
    if ci_to_slot.is_empty() {
        return Vec::new();
    }

    let mut counts = vec![0u32; SENTINELS.len()];
    let mut totals = vec![0u64; SENTINELS.len()];
    for (obj_idx, &ci) in g.class_idx.iter().enumerate() {
        if let Some(&slot) = ci_to_slot.get(&ci) {
            counts[slot] += 1;
            if let Some(&r) = g.retained.get(obj_idx) {
                totals[slot] += r;
            }
        }
    }

    SENTINELS
        .iter()
        .enumerate()
        .filter(|&(slot, _)| counts[slot] > 0)
        .map(|(slot, &(_, label))| FrameworkAnalysis {
            framework: label.to_string(),
            instance_count: counts[slot],
            total_retained: totals[slot],
        })
        .collect()
}

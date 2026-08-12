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
    // Build a Vec<u8> indexed by class index: value = sentinel_slot + 1, 0 = not a sentinel.
    // Fits in L1 cache (class_count is O(thousands)), eliminating hash overhead on the
    // 514M-iteration inner loop compared to a HashMap<u32, usize> lookup per object.
    let class_count = g.class_names.len();
    let mut ci_to_slot: Vec<u8> = vec![0u8; class_count];
    let mut any_sentinel = false;
    for (slot, &(sentinel, _)) in SENTINELS.iter().enumerate() {
        for (i, name) in g.class_names.iter().enumerate() {
            if name.as_str() == sentinel {
                ci_to_slot[i] = (slot + 1) as u8;
                any_sentinel = true;
            }
        }
    }
    if !any_sentinel {
        return Vec::new();
    }

    let mut counts = vec![0u32; SENTINELS.len()];
    let mut totals = vec![0u64; SENTINELS.len()];
    for (obj_idx, &ci) in g.class_idx.iter().enumerate() {
        let ci_usize = ci as usize;
        if ci_usize < class_count {
            let tag = ci_to_slot[ci_usize];
            if tag != 0 {
                let slot = (tag - 1) as usize;
                counts[slot] += 1;
                if let Some(&r) = g.retained.get(obj_idx) {
                    totals[slot] += r;
                }
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

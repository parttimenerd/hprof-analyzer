use crate::pass2::model::Graph;
use crate::report::FrameworkAnalysis;

/// Sentinels: (jvm_class_name_with_slashes, framework_label)
static SENTINELS: &[(&str, &str)] = &[
    ("org/hibernate/internal/SessionImpl", "Hibernate"),
    (
        "org/springframework/context/support/AbstractApplicationContext",
        "Spring",
    ),
    ("java/util/concurrent/ThreadPoolExecutor", "ThreadPoolExecutor"),
    ("io/netty/buffer/AbstractReferenceCountedByteBuf", "Netty"),
    ("com/zaxxer/hikari/pool/HikariPool", "HikariCP"),
];

/// Scan objects of each sentinel class (or its exact name match in class_names).
/// For each detected framework: count instances and sum retained heap.
/// Requires `g.retained` to be populated (called from build_model after retained stage).
pub fn scan_frameworks(g: &Graph) -> Vec<FrameworkAnalysis> {
    let mut results = Vec::new();

    for &(sentinel, label) in SENTINELS {
        // Find class index rows matching the sentinel (may be multiple due to class loaders)
        let matching_ci: Vec<u32> = g
            .class_names
            .iter()
            .enumerate()
            .filter(|(_, name)| name.as_str() == sentinel)
            .map(|(i, _)| i as u32)
            .collect();

        if matching_ci.is_empty() {
            continue;
        }
        let ci_set: std::collections::HashSet<u32> =
            matching_ci.into_iter().collect();

        // Count objects and sum retained
        let mut count = 0u32;
        let mut total_retained = 0u64;

        for (obj_idx, &ci) in g.class_idx.iter().enumerate() {
            if ci_set.contains(&ci) {
                count += 1;
                if let Some(&r) = g.retained.get(obj_idx) {
                    total_retained += r;
                }
            }
        }

        if count == 0 {
            continue;
        }

        results.push(FrameworkAnalysis {
            framework: label.to_string(),
            instance_count: count,
            total_retained,
        });
    }

    results
}

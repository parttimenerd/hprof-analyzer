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

/// Requires `g.retained` to be populated (call after the retained-size stage).
pub fn scan_frameworks(g: &Graph) -> Vec<FrameworkAnalysis> {
    let mut results = Vec::new();

    for &(sentinel, label) in SENTINELS {
        // Collect matching class indices — may be >1 when multiple class loaders define the same class.
        let ci_set: Vec<u32> = g
            .class_names
            .iter()
            .enumerate()
            .filter(|(_, name)| name.as_str() == sentinel)
            .map(|(i, _)| i as u32)
            .collect();

        if ci_set.is_empty() {
            continue;
        }

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

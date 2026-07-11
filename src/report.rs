//! Report generation: system overview, leak suspects, top consumers.

use crate::pass2::Graph;

#[inline]
fn class_obj_repr(g: &Graph, i: usize) -> u32 {
    g.class_obj_class_idx.get(&(i as u32)).copied().unwrap_or(u32::MAX)
}

// ── Formatting helpers ─────────────────────────────────────────────────────

/// ISO-8601 UTC timestamp matching java.time.Instant.toString() shape.
/// Non-deterministic — parity comparison ignores this line.
pub fn now_iso8601() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    let nanos = now.subsec_nanos();

    let days = secs / 86_400;
    let rem = secs % 86_400;
    let (hh, mm, ss) = (rem / 3600, (rem % 3600) / 60, rem % 60);

    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days as i64 + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };

    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:09}Z",
        year, m, d, hh, mm, ss, nanos
    )
}

pub fn format_bytes(n: u64) -> String {
    if n < 1024 {
        return format!("{} B", n);
    }
    if n < 1024 * 1024 {
        return format!("{:.1} KB", n as f64 / 1024.0);
    }
    if n < 1024 * 1024 * 1024 {
        return format!("{:.1} MB", n as f64 / (1024.0 * 1024.0));
    }
    format!("{:.2} GB", n as f64 / (1024.0 * 1024.0 * 1024.0))
}

fn fmt_count(n: u64) -> String {
    let s = n.to_string();
    let mut result = String::new();
    for (i, c) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            result.push(',');
        }
        result.push(c);
    }
    result.chars().rev().collect()
}

pub fn pretty_class_name(raw: &str) -> String {
    if raw.is_empty() {
        return raw.to_string();
    }
    if !raw.starts_with('[') {
        return raw.replace('/', ".");
    }

    let dims = raw.chars().take_while(|&c| c == '[').count();
    let rest = &raw[dims..];

    let base = if rest.len() == 1 {
        match rest.chars().next().unwrap() {
            'Z' => "boolean",
            'B' => "byte",
            'C' => "char",
            'S' => "short",
            'I' => "int",
            'J' => "long",
            'F' => "float",
            'D' => "double",
            _ => rest,
        }
        .to_string()
    } else if rest.starts_with('L') && rest.ends_with(';') {
        rest[1..rest.len() - 1].replace('/', ".")
    } else {
        rest.replace('/', ".")
    };

    format!("{}{}", base, "[]".repeat(dims))
}

fn top_package(name: &str) -> String {
    let mut s = name;
    while s.starts_with('[') {
        s = &s[1..];
    }
    if s.starts_with('L') && s.ends_with(';') {
        s = &s[1..s.len() - 1];
    }
    if s.is_empty() || matches!(s, "B" | "C" | "D" | "F" | "I" | "J" | "S" | "Z") {
        return "(primitives)".to_string();
    }
    if s.ends_with("[]") {
        return "(primitives)".to_string();
    }
    let s = s.replace('/', ".");
    match s.find('.') {
        Some(dot) => s[..dot].to_string(),
        None => s,
    }
}

// ── System Overview ────────────────────────────────────────────────────────

pub fn system_overview(g: &Graph) -> String {
    let n = g.n;
    let undef = u32::MAX;

    // Count reachable objects and total shallow; track unreachable in the same loop.
    let mut total_objects: u64 = 0;
    let mut total_shallow: u64 = 0;
    let mut unreachable_count: u64 = 0;
    let mut unreachable_shallow: u64 = 0;
    for i in 0..n {
        if g.idom[i] != undef {
            total_objects += 1;
            total_shallow += g.shallow[i] as u64;
        } else {
            unreachable_count += 1;
            unreachable_shallow += g.shallow[i] as u64;
        }
    }

    let gc_roots = (g.gc_root_indices.len().saturating_sub(g.synthetic_root_count)) as u64;
    // Count reachable class-dump objects (objects that ARE Java classes, with defined idom)
    let undef_u32 = u32::MAX;
    let classes_loaded = (0..n)
        .filter(|&i| class_obj_repr(g, i) != u32::MAX && g.idom[i] != undef_u32)
        .count() as u64;

    // Class histogram: per-class instance count, shallow total, retained total
    let class_count = g.class_names.len();
    let mut inst_count: Vec<u64> = vec![0; class_count];
    let mut shallow_total: Vec<u64> = vec![0; class_count];
    let mut class_retained: Vec<u64> = vec![0; class_count];

    // First pass: for all reachable objects
    for i in 0..n {
        if g.idom[i] == undef {
            continue;
        }
        let ci = g.class_idx[i] as usize;
        if ci >= class_count {
            continue;
        }
        inst_count[ci] += 1;
        shallow_total[ci] += g.shallow[i] as u64;
        // MAT top-ancestor semantics: only count retained of objects with no
        // same-class (or class-object) ancestor in the dominator tree.
        if !g.has_same_class_ancestor.get(i) {
            class_retained[ci] += g.retained[i];
        }
    }

    // Second pass: for each class object, add its retained to the class it represents
    for i in 0..n {
        if g.idom[i] == undef {
            continue;
        }
        let repr = class_obj_repr(g, i);
        if repr == undef {
            continue;
        }
        let ci = repr as usize;
        if ci >= class_count {
            continue;
        }
        class_retained[ci] += g.retained[i];
    }

    // Sort classes by retained desc, take top 50
    let mut order: Vec<usize> = (0..class_count).collect();
    order.sort_unstable_by(|&a, &b| class_retained[b].cmp(&class_retained[a]));
    let top50 = order.into_iter().take(50);

    let mut out = String::new();
    out.push_str(&format!("# Heap Dump Analysis: `{}`\n\n", g.source_name));
    out.push_str(&format!(
        "*Generated by hprof-redact views — {}*\n\n",
        crate::report::now_iso8601()
    ));
    out.push_str("----\n\n");
    out.push_str("## System Overview\n\n");
    out.push_str("### Heap Summary\n\n");
    out.push_str("| Property | Value |\n");
    out.push_str("|---|---|\n");
    out.push_str(&format!("| HPROF format | {} |\n", g.format));
    out.push_str(&format!("| File size | {} |\n", format_bytes(g.file_size)));
    out.push_str(&format!("| Total objects | {} |\n", fmt_count(total_objects)));
    out.push_str(&format!("| Total shallow heap | {} |\n", format_bytes(total_shallow)));
    out.push_str(&format!("| GC roots | {} |\n", fmt_count(gc_roots)));
    out.push_str(&format!("| Classes loaded | {} |\n", fmt_count(classes_loaded)));
    if unreachable_count > 0 {
        out.push_str(&format!(
            "| Unreachable objects (excluded) | {} ({}) |\n",
            fmt_count(unreachable_count),
            format_bytes(unreachable_shallow),
        ));
    }
    out.push('\n');

    out.push_str("### Class Histogram (by Retained Heap)\n\n");
    out.push_str("| # | Class | Instances | Shallow Heap | Retained Heap |\n");
    out.push_str("|---|---|---:|---:|---:|\n");
    for (rank, ci) in top50.enumerate() {
        let pretty = pretty_class_name(&g.class_names[ci]);
        out.push_str(&format!(
            "| {} | `{}` | {} | {} | {} |\n",
            rank + 1,
            pretty,
            fmt_count(inst_count[ci]),
            format_bytes(shallow_total[ci]),
            fmt_count(class_retained[ci]),
        ));
    }
    out.push('\n');

    out
}

// ── Leak Suspects ─────────────────────────────────────────────────────────

const THRESHOLD_PCT: f64 = 10.0;

pub fn leak_suspects(g: &Graph, dc_offsets: &[u32], dc_targets: &[u32]) -> String {
    let n = g.n;
    let undef = u32::MAX;

    // Total shallow heap of reachable objects
    let total_shallow: u64 = (0..n)
        .filter(|&i| g.idom[i] != undef)
        .map(|i| g.shallow[i] as u64)
        .sum();

    let threshold = (total_shallow as f64 * THRESHOLD_PCT / 100.0) as u64;

    // The dominator-children CSR (dc_offsets/dc_targets) is built ONCE in main
    // by retained::build_dom_children_csr and shared with compute_retained.
    let dom_children = |node: usize| -> &[u32] {
        &dc_targets[dc_offsets[node] as usize..dc_offsets[node + 1] as usize]
    };

    struct Suspect {
        is_single: bool,
        obj_idx: u32,            // only meaningful for single
        class_idx: usize,
        instance_count: u64,
        retained: u64,
        shallow: u64,
    }

    let mut suspects: Vec<Suspect> = Vec::new();
    let mut single_class_set: std::collections::HashSet<usize> = std::collections::HashSet::new();

    // Phase 1: single objects directly dominated by vroot with retained >= threshold
    for &i in dom_children(n) {
        let idx = i as usize;
        if g.retained[idx] >= threshold {
            let ci = g.class_idx[idx] as usize;
            single_class_set.insert(ci);
            suspects.push(Suspect {
                is_single: true,
                obj_idx: i,
                class_idx: ci,
                instance_count: 1,
                retained: g.retained[idx],
                shallow: g.shallow[idx] as u64,
            });
        }
    }

    // Phase 2: class groups of top-level dominators
    let class_count = g.class_names.len();
    let mut group_retained: Vec<u64> = vec![0; class_count];
    let mut group_count: Vec<u64> = vec![0; class_count];
    let mut group_shallow: Vec<u64> = vec![0; class_count];
    for &i in dom_children(n) {
        let idx = i as usize;
        let ci = g.class_idx[idx] as usize;
        if ci < class_count {
            group_retained[ci] += g.retained[idx];
            group_count[ci] += 1;
            group_shallow[ci] += g.shallow[idx] as u64;
        }
    }
    for ci in 0..class_count {
        if group_retained[ci] >= threshold && !single_class_set.contains(&ci) {
            suspects.push(Suspect {
                is_single: false,
                obj_idx: u32::MAX,
                class_idx: ci,
                instance_count: group_count[ci],
                retained: group_retained[ci],
                shallow: group_shallow[ci],
            });
        }
    }

    // Sort by retained desc
    suspects.sort_unstable_by(|a, b| b.retained.cmp(&a.retained));

    let mut out = String::new();
    out.push_str("## Leak Suspects\n\n");

    if suspects.is_empty() {
        out.push_str("No single object or class group exceeds the threshold.\n\n");
        return out;
    }

    for (rank, s) in suspects.iter().enumerate() {
        let pretty = pretty_class_name(&g.class_names[s.class_idx]);
        let pct = if total_shallow > 0 {
            s.retained as f64 / total_shallow as f64 * 100.0
        } else {
            0.0
        };
        let type_label = if s.is_single {
            "Single large object"
        } else {
            "Class group"
        };

        out.push_str(&format!("### Suspect {}: `{}`\n\n", rank + 1, pretty));
        out.push_str(&format!("- **Type**: {}\n", type_label));
        out.push_str(&format!("- **Instances**: {}\n", fmt_count(s.instance_count)));
        out.push_str(&format!(
            "- **Retained heap**: {} ({:.1}% of total)\n",
            format_bytes(s.retained),
            pct
        ));
        out.push_str(&format!("- **Shallow heap**: {}\n", format_bytes(s.shallow)));
        out.push('\n');

        // Accumulation path for single suspects
        if s.is_single {
            out.push_str("**Accumulation point path** (largest retained child at each step):\n\n");
            out.push_str("| Depth | Object Index | Class | Retained |\n");
            out.push_str("|---|---|---|---:|\n");

            let mut cur = s.obj_idx as usize;
            for depth in 0..=5 {
                let ci = g.class_idx[cur] as usize;
                // For class objects, show the class they represent (MAT parity: no "class " prefix)
                let display_class = if class_obj_repr(g, cur) != u32::MAX {
                    let repr = class_obj_repr(g, cur) as usize;
                    if repr < g.class_names.len() {
                        pretty_class_name(&g.class_names[repr])
                    } else {
                        pretty_class_name(&g.class_names[ci])
                    }
                } else if ci < g.class_names.len() {
                    pretty_class_name(&g.class_names[ci])
                } else {
                    String::from("?")
                };

                out.push_str(&format!(
                    "| {} | {} | `{}` | {} |\n",
                    depth,
                    cur + 1,
                    display_class,
                    format_bytes(g.retained[cur]),
                ));

                // Find child with max retained
                let best_child = dom_children(cur)
                    .iter()
                    .max_by_key(|&&c| g.retained[c as usize]);
                match best_child {
                    Some(&c) => cur = c as usize,
                    None => break,
                }
            }
            out.push('\n');
        }
    }

    out
}

// ── Top Consumers ─────────────────────────────────────────────────────────

const TOP_N: usize = 20;

pub fn top_consumers(g: &Graph) -> String {
    let n = g.n;
    let vroot = n as u32;
    let undef = u32::MAX;
    let class_count = g.class_names.len();

    // Collect top-level dominators
    let mut top_level: Vec<u32> = Vec::new();
    for i in 0..n {
        if g.idom[i] == vroot {
            top_level.push(i as u32);
        }
    }

    // Total shallow of all reachable objects (MAT parity: pct base for Biggest Objects)
    let total_shallow: u64 = (0..n)
        .filter(|&i| g.idom[i] != undef)
        .map(|i| g.shallow[i] as u64)
        .sum();

    // Sort by retained desc for biggest objects
    let mut sorted_top: Vec<u32> = top_level.clone();
    sorted_top.sort_unstable_by(|&a, &b| g.retained[b as usize].cmp(&g.retained[a as usize]));

    // Biggest Objects
    let mut out = String::new();
    out.push_str("## Top Consumers\n\n");
    out.push_str("### Biggest Objects (Top-Level Dominators)\n\n");
    out.push_str("| # | Object Index | Class | Shallow | Retained |\n");
    out.push_str("|---|---|---|---:|---:|\n");

    for (rank, &i) in sorted_top.iter().take(TOP_N).enumerate() {
        let idx = i as usize;
        let ci = g.class_idx[idx] as usize;
        // For class objects, show the class they represent (MAT parity: no "class " prefix)
        let display_class = if class_obj_repr(g, idx) != undef {
            let repr = class_obj_repr(g, idx) as usize;
            if repr < g.class_names.len() {
                pretty_class_name(&g.class_names[repr])
            } else if ci < g.class_names.len() {
                pretty_class_name(&g.class_names[ci])
            } else {
                String::from("?")
            }
        } else if ci < g.class_names.len() {
            pretty_class_name(&g.class_names[ci])
        } else {
            String::from("?")
        };

        let pct = if total_shallow > 0 {
            g.retained[idx] as f64 / total_shallow as f64 * 100.0
        } else {
            0.0
        };

        out.push_str(&format!(
            "| {} | {} | `{}` | {} | {} ({:.1}%) |\n",
            rank + 1,
            idx + 1,
            display_class,
            format_bytes(g.shallow[idx] as u64),
            format_bytes(g.retained[idx]),
            pct,
        ));
    }
    out.push('\n');

    // Biggest Classes by Retained Heap
    let mut class_retained: Vec<u64> = vec![0; class_count];
    let mut class_count_map: Vec<u64> = vec![0; class_count];
    for &i in &top_level {
        let idx = i as usize;
        let ci = g.class_idx[idx] as usize;
        if ci < class_count {
            class_retained[ci] += g.retained[idx];
            class_count_map[ci] += 1;
        }
    }
    let mut class_order: Vec<usize> = (0..class_count)
        .filter(|&ci| class_retained[ci] > 0)
        .collect();
    class_order.sort_unstable_by(|&a, &b| class_retained[b].cmp(&class_retained[a]));

    out.push_str("### Biggest Classes by Retained Heap\n\n");
    out.push_str("| # | Class | Instances | Retained Heap |\n");
    out.push_str("|---|---|---:|---:|\n");
    for (rank, ci) in class_order.iter().take(TOP_N).enumerate() {
        let pretty = pretty_class_name(&g.class_names[*ci]);
        out.push_str(&format!(
            "| {} | `{}` | {} | {} |\n",
            rank + 1,
            pretty,
            fmt_count(class_count_map[*ci]),
            format_bytes(class_retained[*ci]),
        ));
    }
    out.push('\n');

    // Biggest Packages
    let mut pkg_retained: std::collections::HashMap<String, u64> =
        std::collections::HashMap::new();
    let mut pkg_count: std::collections::HashMap<String, u64> =
        std::collections::HashMap::new();
    for &i in &top_level {
        let idx = i as usize;
        // Use the class the object represents (for class objects), else own class
        let raw_name = if class_obj_repr(g, idx) != undef {
            let repr = class_obj_repr(g, idx) as usize;
            if repr < g.class_names.len() {
                &g.class_names[repr]
            } else {
                let ci = g.class_idx[idx] as usize;
                if ci < g.class_names.len() {
                    &g.class_names[ci]
                } else {
                    continue;
                }
            }
        } else {
            let ci = g.class_idx[idx] as usize;
            if ci < g.class_names.len() {
                &g.class_names[ci]
            } else {
                continue;
            }
        };
        let pkg = top_package(raw_name);
        *pkg_retained.entry(pkg.clone()).or_insert(0) += g.retained[idx];
        *pkg_count.entry(pkg).or_insert(0) += 1;
    }
    let mut pkg_order: Vec<(String, u64, u64)> = pkg_retained
        .iter()
        .map(|(k, &v)| (k.clone(), v, *pkg_count.get(k).unwrap_or(&0)))
        .collect();
    pkg_order.sort_unstable_by(|a, b| b.1.cmp(&a.1));

    out.push_str("### Biggest Packages by Retained Heap\n\n");
    out.push_str("| # | Package | Objects | Retained Heap |\n");
    out.push_str("|---|---|---:|---:|\n");
    for (rank, (pkg, retained, count)) in pkg_order.iter().take(TOP_N).enumerate() {
        out.push_str(&format!(
            "| {} | `{}` | {} | {} |\n",
            rank + 1,
            pkg,
            fmt_count(*count),
            format_bytes(*retained),
        ));
    }
    out.push('\n');

    out
}

// ── Unit tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_bytes() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(1024), "1.0 KB");
        assert_eq!(format_bytes(1024 * 1024), "1.0 MB");
        assert_eq!(format_bytes(1024 * 1024 * 1024), "1.00 GB");
    }

    #[test]
    fn test_fmt_count() {
        assert_eq!(fmt_count(0), "0");
        assert_eq!(fmt_count(999), "999");
        assert_eq!(fmt_count(1000), "1,000");
        assert_eq!(fmt_count(1_000_000), "1,000,000");
        assert_eq!(fmt_count(2_698_510), "2,698,510");
    }

    #[test]
    fn test_pretty_class_name() {
        assert_eq!(pretty_class_name("java/lang/String"), "java.lang.String");
        assert_eq!(pretty_class_name("[I"), "int[]");
        assert_eq!(pretty_class_name("[B"), "byte[]");
        assert_eq!(pretty_class_name("[Ljava/lang/String;"), "java.lang.String[]");
        assert_eq!(pretty_class_name("[[I"), "int[][]");
        assert_eq!(pretty_class_name("[Z"), "boolean[]");
        assert_eq!(pretty_class_name("[C"), "char[]");
    }

    #[test]
    fn test_top_package() {
        assert_eq!(top_package("java/lang/String"), "java");
        assert_eq!(top_package("com/example/Foo"), "com");
        assert_eq!(top_package("[I"), "(primitives)");
        assert_eq!(top_package("[B"), "(primitives)");
        assert_eq!(top_package("[Ljava/lang/String;"), "java");
    }
}

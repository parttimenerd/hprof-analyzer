//! Report generation: system overview, leak suspects, top consumers.
//!
//! Rendering goes through an explicit data model: `build_model` reads the
//! `Graph` (including the large per-object arrays) and computes only bounded
//! aggregates into a `Report`; `render_markdown` formats a `Report` into the
//! Markdown output. This keeps peak RSS bounded (the model never stores a
//! per-object Vec) and makes ordering deterministic.

use crate::pass2::Graph;

#[inline]
fn class_obj_repr(g: &Graph, i: usize) -> u32 {
    g.class_obj_class_idx
        .get(&(i as u32))
        .copied()
        .unwrap_or(u32::MAX)
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

// ── Data model ──────────────────────────────────────────────────────────────

const THRESHOLD_PCT: f64 = 10.0;
const TOP_N: usize = 20;
/// If the single largest suspect retains at least this share of the reachable
/// heap, the OOM-triage lead-in calls the heap "dominated" by one retainer.
const CONCENTRATION_PCT: f64 = 50.0;

/// One row of the System-Overview class histogram (top 50 by retained).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct HistRow {
    pub pretty_class: String,
    pub instances: u64,
    pub shallow: u64,
    pub retained: u64,
}

/// Aggregates for the "System Overview" section.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct SystemOverview {
    pub source_name: String,
    pub format: String,
    pub file_size: u64,
    pub total_objects: u64,
    pub total_shallow: u64,
    pub gc_roots: u64,
    pub classes_loaded: u64,
    pub unreachable_count: u64,
    pub unreachable_shallow: u64,
    pub histogram: Vec<HistRow>,
}

/// One step of a single-suspect accumulation path.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct PathStep {
    pub depth: usize,
    pub obj_index_1based: usize,
    pub display_class: String,
    pub retained: u64,
}

/// One leak suspect (single large object or class group).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct Suspect {
    pub is_single: bool,
    pub pretty_class: String,
    pub instance_count: u64,
    pub retained: u64,
    pub shallow: u64,
    /// Non-empty only for single suspects.
    pub path: Vec<PathStep>,
}

/// Aggregates for the "Leak Suspects" section.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct LeakSuspects {
    pub total_shallow: u64,
    pub suspects: Vec<Suspect>,
}

/// One row of "Biggest Objects".
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct ObjRow {
    pub obj_index_1based: usize,
    pub display_class: String,
    pub shallow: u64,
    pub retained: u64,
    /// Retained share of total reachable shallow heap, in integer basis
    /// points (bp = round(retained / total_shallow * 10000)). Deterministic
    /// integer for JSON output; the Markdown renderer uses `pct` instead.
    pub pct_bp: u64,
    /// Retained share as a percentage (0..=100), used only for Markdown
    /// formatting. Skipped from JSON/schema because f64 is a
    /// determinism/precision risk in the machine-readable output.
    #[serde(skip)]
    #[schemars(skip)]
    pub pct: f64,
}

/// One row of "Biggest Classes".
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct ClassRow {
    pub pretty_class: String,
    pub instances: u64,
    pub retained: u64,
}

/// One row of "Biggest Packages".
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct PkgRow {
    pub package: String,
    pub objects: u64,
    pub retained: u64,
}

/// Aggregates for the "Top Consumers" section.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct TopConsumers {
    pub biggest_objects: Vec<ObjRow>,
    pub biggest_classes: Vec<ClassRow>,
    pub biggest_packages: Vec<PkgRow>,
}

/// Schema version for the machine-readable JSON output. Bump on any
/// breaking change to the `Report` shape; the JSON always carries this.
pub const SCHEMA_VERSION: u32 = 1;

/// Full report data model: only bounded aggregates, never a per-object Vec.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct Report {
    pub schema_version: u32,
    pub generated: String,
    pub overview: SystemOverview,
    pub leaks: LeakSuspects,
    pub top: TopConsumers,
}

// ── Model construction ───────────────────────────────────────────────────────

/// Compute all report aggregates from the graph.
///
/// Ordering mirrors the previous three separate render calls so callers keep
/// the same free-as-you-go RSS discipline: the system-overview group is
/// computed first (the only reader of `has_same_class_ancestor`), then the
/// leak-suspect group (the only reader of `dc_offsets`/`dc_targets`), then top
/// consumers. Because the returned `Report` holds only small aggregates, the
/// caller may free `has_same_class_ancestor` and `dc_offsets`/`dc_targets`
/// immediately after this returns.
pub fn build_model(g: &Graph, dc_offsets: &[u32], dc_targets: &[u32]) -> Report {
    let generated = now_iso8601();
    crate::trace::probe("build_model: before system_overview aggregates");
    let overview = build_system_overview(g);
    crate::trace::probe("build_model: after system_overview aggregates");
    let leaks = build_leak_suspects(g, dc_offsets, dc_targets);
    crate::trace::probe("build_model: after leak_suspects aggregates");
    let top = build_top_consumers(g);
    crate::trace::probe("build_model: after top_consumers aggregates");
    Report {
        schema_version: SCHEMA_VERSION,
        generated,
        overview,
        leaks,
        top,
    }
}

fn build_system_overview(g: &Graph) -> SystemOverview {
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

    let gc_roots = (g
        .gc_root_indices
        .len()
        .saturating_sub(g.synthetic_root_count)) as u64;
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

    // Sort classes by retained desc, take top 50. Explicit tie-breaker on
    // ascending class index so equal-retained rows are deterministic.
    let mut order: Vec<usize> = (0..class_count).collect();
    order.sort_unstable_by(|&a, &b| class_retained[b].cmp(&class_retained[a]).then(a.cmp(&b)));
    let histogram: Vec<HistRow> = order
        .into_iter()
        .take(50)
        .map(|ci| HistRow {
            pretty_class: pretty_class_name(&g.class_names[ci]),
            instances: inst_count[ci],
            shallow: shallow_total[ci],
            retained: class_retained[ci],
        })
        .collect();

    SystemOverview {
        source_name: g.source_name.clone(),
        format: g.format.clone(),
        file_size: g.file_size,
        total_objects,
        total_shallow,
        gc_roots,
        classes_loaded,
        unreachable_count,
        unreachable_shallow,
        histogram,
    }
}

fn build_leak_suspects(g: &Graph, dc_offsets: &[u32], dc_targets: &[u32]) -> LeakSuspects {
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

    struct RawSuspect {
        is_single: bool,
        obj_idx: u32, // only meaningful for single
        class_idx: usize,
        instance_count: u64,
        retained: u64,
        shallow: u64,
    }

    let mut suspects: Vec<RawSuspect> = Vec::new();
    let mut single_class_set: std::collections::HashSet<usize> = std::collections::HashSet::new();

    // Phase 1: single objects directly dominated by vroot with retained >= threshold
    for &i in dom_children(n) {
        let idx = i as usize;
        if g.retained[idx] >= threshold {
            let ci = g.class_idx[idx] as usize;
            single_class_set.insert(ci);
            suspects.push(RawSuspect {
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
            suspects.push(RawSuspect {
                is_single: false,
                obj_idx: u32::MAX,
                class_idx: ci,
                instance_count: group_count[ci],
                retained: group_retained[ci],
                shallow: group_shallow[ci],
            });
        }
    }

    // Sort by retained desc, with explicit tie-breaker on (class_idx asc,
    // obj_idx asc) so equal-retained suspects are deterministic.
    suspects.sort_unstable_by(|a, b| {
        b.retained
            .cmp(&a.retained)
            .then(a.class_idx.cmp(&b.class_idx))
            .then(a.obj_idx.cmp(&b.obj_idx))
    });

    // Materialise into the model, resolving the accumulation path for singles.
    let out: Vec<Suspect> = suspects
        .iter()
        .map(|s| {
            let mut path: Vec<PathStep> = Vec::new();
            if s.is_single {
                let mut cur = s.obj_idx as usize;
                for depth in 0..=5 {
                    let ci = g.class_idx[cur] as usize;
                    // For class objects, show the class they represent (MAT
                    // parity: no "class " prefix)
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

                    path.push(PathStep {
                        depth,
                        obj_index_1based: cur + 1,
                        display_class,
                        retained: g.retained[cur],
                    });

                    // Find child with max retained
                    let best_child = dom_children(cur)
                        .iter()
                        .max_by_key(|&&c| g.retained[c as usize]);
                    match best_child {
                        Some(&c) => cur = c as usize,
                        None => break,
                    }
                }
            }
            Suspect {
                is_single: s.is_single,
                pretty_class: pretty_class_name(&g.class_names[s.class_idx]),
                instance_count: s.instance_count,
                retained: s.retained,
                shallow: s.shallow,
                path,
            }
        })
        .collect();

    LeakSuspects {
        total_shallow,
        suspects: out,
    }
}

fn build_top_consumers(g: &Graph) -> TopConsumers {
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

    // Sort by retained desc for biggest objects, with tie-breaker on ascending
    // object index (top_level built in ascending order).
    let mut sorted_top: Vec<u32> = top_level.clone();
    sorted_top.sort_unstable_by(|&a, &b| {
        g.retained[b as usize]
            .cmp(&g.retained[a as usize])
            .then(a.cmp(&b))
    });

    // Biggest Objects
    let biggest_objects: Vec<ObjRow> = sorted_top
        .iter()
        .take(TOP_N)
        .map(|&i| {
            let idx = i as usize;
            let ci = g.class_idx[idx] as usize;
            // For class objects, show the class they represent (MAT parity: no
            // "class " prefix)
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
            // Integer basis points of the retained share, for deterministic
            // JSON output (round-half-to-even via f64::round on *10000).
            let pct_bp = if total_shallow > 0 {
                (g.retained[idx] as f64 / total_shallow as f64 * 10000.0).round() as u64
            } else {
                0
            };

            ObjRow {
                obj_index_1based: idx + 1,
                display_class,
                shallow: g.shallow[idx] as u64,
                retained: g.retained[idx],
                pct_bp,
                pct,
            }
        })
        .collect();

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
    // Retained desc, tie-breaker ascending class index.
    class_order
        .sort_unstable_by(|&a, &b| class_retained[b].cmp(&class_retained[a]).then(a.cmp(&b)));
    let biggest_classes: Vec<ClassRow> = class_order
        .iter()
        .take(TOP_N)
        .map(|&ci| ClassRow {
            pretty_class: pretty_class_name(&g.class_names[ci]),
            instances: class_count_map[ci],
            retained: class_retained[ci],
        })
        .collect();

    // Biggest Packages
    let mut pkg_retained: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
    let mut pkg_count: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
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
    // HashMap iteration is nondeterministic, so sort by retained desc AND
    // package-name ascending to make the collect order and tie rows stable.
    pkg_order.sort_unstable_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let biggest_packages: Vec<PkgRow> = pkg_order
        .iter()
        .take(TOP_N)
        .map(|(pkg, retained, count)| PkgRow {
            package: pkg.clone(),
            objects: *count,
            retained: *retained,
        })
        .collect();

    TopConsumers {
        biggest_objects,
        biggest_classes,
        biggest_packages,
    }
}

// ── Rendering ────────────────────────────────────────────────────────────────

/// Render a `Report` into Markdown. Byte-identical to the previous
/// `system_overview` + `leak_suspects` + `top_consumers` concatenation.
pub fn render_markdown(r: &Report) -> String {
    let mut out = String::new();
    render_title(&r.overview, &r.generated, &mut out);
    render_oom_triage(r, &mut out);
    render_system_overview(&r.overview, &mut out);
    render_leak_suspects(&r.leaks, &mut out);
    render_top_consumers(&r.top, r.leaks.total_shallow, &mut out);
    out
}

/// Emit the document title + generation timestamp + horizontal rule.
/// Split out of `render_system_overview` so the OOM-triage lead-in can sit
/// between the title and the first section.
fn render_title(o: &SystemOverview, generated: &str, out: &mut String) {
    out.push_str(&format!("# Heap Dump Analysis: `{}`\n\n", o.source_name));
    out.push_str(&format!(
        "*Generated by hprof-redact views — {}*\n\n",
        generated
    ));
    out.push_str("----\n\n");
}

/// OOM-triage lead-in: a short, human-readable summary re-projecting data
/// already in the model (no new model fields). Names the dominant retainer
/// and characterises how concentrated retention is. Pure function of `Report`.
fn render_oom_triage(r: &Report, out: &mut String) {
    out.push_str("## OOM Triage\n\n");
    out.push_str("_Where the reachable heap is concentrated, at a glance._\n\n");

    // Percentage basis matches the existing tables: retained / total reachable
    // shallow heap. Reuse the leak-suspects total (identical to top-consumers').
    let total = r.leaks.total_shallow;
    let pct_of = |retained: u64| -> f64 {
        if total > 0 {
            retained as f64 / total as f64 * 100.0
        } else {
            0.0
        }
    };

    // Headline retainer: prefer the #1 leak suspect; fall back to the biggest
    // top-level consumer; otherwise report that nothing dominates.
    if let Some(s) = r.leaks.suspects.first() {
        let kind = if s.is_single {
            "a single object"
        } else {
            "a class group"
        };
        out.push_str(&format!(
            "- **Headline retainer:** `{}` ({}) retains {} ({:.1}% of reachable heap).\n",
            s.pretty_class,
            kind,
            format_bytes(s.retained),
            pct_of(s.retained),
        ));
    } else if let Some(o) = r.top.biggest_objects.first() {
        out.push_str(&format!(
            "- **Headline retainer:** `{}` (object #{}) retains {} ({:.1}% of reachable heap).\n",
            o.display_class,
            o.obj_index_1based,
            format_bytes(o.retained),
            pct_of(o.retained),
        ));
    } else {
        out.push_str("- **Headline retainer:** No dominant retainer found.\n");
    }

    // Concentration hint: derived purely from the suspects list.
    match r.leaks.suspects.first() {
        Some(s) if pct_of(s.retained) >= CONCENTRATION_PCT => {
            out.push_str(&format!(
                "- **Concentration:** A single object/class group dominates the heap ({:.1}%).\n",
                pct_of(s.retained),
            ));
        }
        Some(_) => {
            out.push_str("- **Concentration:** Retention is spread across multiple roots.\n");
        }
        None => {
            out.push_str(
                "- **Concentration:** No suspect exceeds the threshold; retention is spread across many roots.\n",
            );
        }
    }
    out.push('\n');
}

fn render_system_overview(o: &SystemOverview, out: &mut String) {
    out.push_str("## System Overview\n\n");
    out.push_str("_Reachable-heap totals and the largest classes by retained heap._\n\n");
    out.push_str("### Heap Summary\n\n");
    out.push_str("| Property | Value |\n");
    out.push_str("|---|---|\n");
    out.push_str(&format!("| HPROF format | {} |\n", o.format));
    out.push_str(&format!("| File size | {} |\n", format_bytes(o.file_size)));
    out.push_str(&format!(
        "| Total objects | {} |\n",
        fmt_count(o.total_objects)
    ));
    out.push_str(&format!(
        "| Total shallow heap | {} |\n",
        format_bytes(o.total_shallow)
    ));
    out.push_str(&format!("| GC roots | {} |\n", fmt_count(o.gc_roots)));
    out.push_str(&format!(
        "| Classes loaded | {} |\n",
        fmt_count(o.classes_loaded)
    ));
    if o.unreachable_count > 0 {
        out.push_str(&format!(
            "| Unreachable objects (excluded) | {} ({}) |\n",
            fmt_count(o.unreachable_count),
            format_bytes(o.unreachable_shallow),
        ));
    }
    out.push('\n');

    out.push_str("### Class Histogram (by Retained Heap)\n\n");
    out.push_str("| # | Class | Instances | Shallow Heap | Retained Heap |\n");
    out.push_str("|---|---|---:|---:|---:|\n");
    for (rank, row) in o.histogram.iter().enumerate() {
        out.push_str(&format!(
            "| {} | `{}` | {} | {} | {} |\n",
            rank + 1,
            row.pretty_class,
            fmt_count(row.instances),
            format_bytes(row.shallow),
            fmt_count(row.retained),
        ));
    }
    out.push('\n');
}

fn render_leak_suspects(l: &LeakSuspects, out: &mut String) {
    out.push_str("## Leak Suspects\n\n");

    if l.suspects.is_empty() {
        out.push_str("No single object or class group exceeds the threshold.\n\n");
        return;
    }

    for (rank, s) in l.suspects.iter().enumerate() {
        let pct = if l.total_shallow > 0 {
            s.retained as f64 / l.total_shallow as f64 * 100.0
        } else {
            0.0
        };
        let type_label = if s.is_single {
            "Single large object"
        } else {
            "Class group"
        };

        out.push_str(&format!(
            "### Suspect {}: `{}`\n\n",
            rank + 1,
            s.pretty_class
        ));
        out.push_str(&format!("- **Type**: {}\n", type_label));
        out.push_str(&format!(
            "- **Instances**: {}\n",
            fmt_count(s.instance_count)
        ));
        out.push_str(&format!(
            "- **Retained heap**: {} ({:.1}% of total)\n",
            format_bytes(s.retained),
            pct
        ));
        out.push_str(&format!(
            "- **Shallow heap**: {}\n",
            format_bytes(s.shallow)
        ));
        out.push('\n');

        // Accumulation path for single suspects
        if s.is_single {
            out.push_str("**Accumulation point path** (largest retained child at each step):\n\n");
            out.push_str("| Depth | Object Index | Class | Retained |\n");
            out.push_str("|---|---|---|---:|\n");

            for step in &s.path {
                out.push_str(&format!(
                    "| {} | {} | `{}` | {} |\n",
                    step.depth,
                    step.obj_index_1based,
                    step.display_class,
                    format_bytes(step.retained),
                ));
            }
            out.push('\n');
        }
    }
}

fn render_top_consumers(t: &TopConsumers, total_shallow: u64, out: &mut String) {
    out.push_str("## Top Consumers\n\n");
    out.push_str("### Biggest Objects (Top-Level Dominators)\n\n");
    out.push_str("| # | Object Index | Class | Shallow | Retained |\n");
    out.push_str("|---|---|---|---:|---:|\n");

    for (rank, row) in t.biggest_objects.iter().enumerate() {
        out.push_str(&format!(
            "| {} | {} | `{}` | {} | {} ({:.1}%) |\n",
            rank + 1,
            row.obj_index_1based,
            row.display_class,
            format_bytes(row.shallow),
            format_bytes(row.retained),
            if total_shallow > 0 {
                row.retained as f64 / total_shallow as f64 * 100.0
            } else {
                0.0
            },
        ));
    }
    out.push('\n');

    out.push_str("### Biggest Classes by Retained Heap\n\n");
    out.push_str("| # | Class | Instances | Retained Heap |\n");
    out.push_str("|---|---|---:|---:|\n");
    for (rank, row) in t.biggest_classes.iter().enumerate() {
        out.push_str(&format!(
            "| {} | `{}` | {} | {} |\n",
            rank + 1,
            row.pretty_class,
            fmt_count(row.instances),
            format_bytes(row.retained),
        ));
    }
    out.push('\n');

    out.push_str("### Biggest Packages by Retained Heap\n\n");
    out.push_str("| # | Package | Objects | Retained Heap |\n");
    out.push_str("|---|---|---:|---:|\n");
    for (rank, row) in t.biggest_packages.iter().enumerate() {
        out.push_str(&format!(
            "| {} | `{}` | {} | {} |\n",
            rank + 1,
            row.package,
            fmt_count(row.objects),
            format_bytes(row.retained),
        ));
    }
    out.push('\n');
}

// ── Unit tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pass2::Graph;
    use std::collections::HashMap;

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
        assert_eq!(
            pretty_class_name("[Ljava/lang/String;"),
            "java.lang.String[]"
        );
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

    /// Build a tiny synthetic Graph plus the dominator-children CSR that
    /// `build_model` expects, for the objects/classes described below.
    ///
    /// Layout (n objects, vroot = n):
    /// - `idom[i]`  = immediate dominator (u32::MAX = unreachable, n = vroot).
    /// - `class_idx[i]` = class-histogram row for object i.
    /// - `shallow[i]`, `retained[i]` as given.
    /// - `class_names` gives the raw JVM names for each class row.
    /// - `class_obj_class_idx` maps class-object index -> represented class row.
    /// - `has_same[i]` marks objects with a same-class ancestor (excluded from
    ///   class_retained accumulation).
    ///
    /// Returns `(Graph, dc_off, dc_tgt)`.
    #[allow(clippy::too_many_arguments)]
    fn make_graph(
        idom: Vec<u32>,
        class_idx: Vec<u32>,
        shallow: Vec<u32>,
        retained: Vec<u64>,
        class_names: Vec<&str>,
        class_obj: &[(u32, u32)],
        has_same_true: &[usize],
        gc_root_indices: Vec<u32>,
        synthetic_root_count: usize,
    ) -> (Graph, Vec<u32>, Vec<u32>) {
        let n = idom.len();
        let mut class_obj_class_idx: HashMap<u32, u32> = HashMap::new();
        for &(k, v) in class_obj {
            class_obj_class_idx.insert(k, v);
        }
        let mut has_same = crate::bitset::Bitset::with_len(n);
        for &i in has_same_true {
            has_same.set(i);
        }

        // Build dominator-children CSR indexed 0..=n (node n = vroot). Children
        // of node p are all i with idom[i] == p, in ascending object order.
        let mut children: Vec<Vec<u32>> = vec![Vec::new(); n + 1];
        for (i, &d) in idom.iter().enumerate() {
            if d == u32::MAX {
                continue;
            }
            children[d as usize].push(i as u32);
        }
        let mut dc_off: Vec<u32> = Vec::with_capacity(n + 2);
        let mut dc_tgt: Vec<u32> = Vec::new();
        dc_off.push(0);
        for kids in &children {
            dc_tgt.extend_from_slice(kids);
            dc_off.push(dc_tgt.len() as u32);
        }

        let g = Graph {
            n,
            format: "JAVA PROFILE 1.0.2".to_string(),
            file_size: 4096,
            source_name: "test.hprof".to_string(),
            gc_root_indices,
            shallow,
            class_idx,
            class_names: class_names.iter().map(|s| s.to_string()).collect(),
            class_obj_class_idx,
            fwd_offsets: Vec::new(),
            fwd_targets: Vec::new(),
            synthetic_root_count,
            idom,
            retained,
            has_same_class_ancestor: has_same,
        };
        (g, dc_off, dc_tgt)
    }

    /// A fixture with 4 reachable objects + 1 unreachable, 3 classes.
    /// - obj0: class0 (com/foo/A), top-level, retained 1000, shallow 100
    /// - obj1: class1 (com/foo/B), top-level, retained 1000, shallow 100 (ties obj0)
    /// - obj2: class0 (com/foo/A), dominated by obj0, retained 50, shallow 50, has_same
    /// - obj3: class2 (org/bar/C), top-level, retained 200, shallow 20
    /// - obj4: class1, UNREACHABLE (idom = MAX), shallow 7
    /// - vroot = 5.
    fn fixture() -> (Graph, Vec<u32>, Vec<u32>) {
        make_graph(
            vec![5, 5, 0, 5, u32::MAX],   // idom
            vec![0, 1, 0, 2, 1],          // class_idx
            vec![100, 100, 50, 20, 7],    // shallow
            vec![1000, 1000, 50, 200, 0], // retained
            vec!["com/foo/A", "com/foo/B", "org/bar/C"],
            &[],           // no class objects
            &[2],          // obj2 has same-class ancestor
            vec![0, 1, 3], // gc roots
            0,             // no synthetic roots
        )
    }

    #[test]
    fn test_build_model_system_overview() {
        let (g, dc_off, dc_tgt) = fixture();
        let r = build_model(&g, &dc_off, &dc_tgt);
        let o = &r.overview;
        assert_eq!(o.total_objects, 4);
        assert_eq!(o.total_shallow, 100 + 100 + 50 + 20);
        assert_eq!(o.unreachable_count, 1);
        assert_eq!(o.unreachable_shallow, 7);
        assert_eq!(o.gc_roots, 3);
        assert_eq!(o.classes_loaded, 0);

        // Histogram: class0 retained = obj0(1000) + obj2 excluded (has_same) = 1000
        //            class1 retained = obj1(1000) = 1000
        //            class2 retained = obj3(200) = 200
        // Sort by retained desc, tie-break ascending class index -> class0, class1, class2.
        assert_eq!(o.histogram.len(), 3);
        assert_eq!(o.histogram[0].pretty_class, "com.foo.A");
        assert_eq!(o.histogram[0].retained, 1000);
        assert_eq!(o.histogram[0].instances, 2); // obj0 + obj2
        assert_eq!(o.histogram[0].shallow, 150);
        assert_eq!(o.histogram[1].pretty_class, "com.foo.B");
        assert_eq!(o.histogram[1].retained, 1000);
        assert_eq!(o.histogram[2].pretty_class, "org.bar.C");
        assert_eq!(o.histogram[2].retained, 200);
    }

    #[test]
    fn test_build_model_top_consumers_package_determinism() {
        let (g, dc_off, dc_tgt) = fixture();
        let r = build_model(&g, &dc_off, &dc_tgt);
        let t = &r.top;

        // Biggest objects: top-level are obj0(1000), obj1(1000), obj3(200).
        // Tie between obj0/obj1 broken by ascending index -> obj0 (index 1), obj1 (index 2).
        assert_eq!(t.biggest_objects.len(), 3);
        assert_eq!(t.biggest_objects[0].obj_index_1based, 1);
        assert_eq!(t.biggest_objects[1].obj_index_1based, 2);
        assert_eq!(t.biggest_objects[2].obj_index_1based, 4);

        // Biggest classes (over top-level only): class0=1000, class1=1000, class2=200.
        assert_eq!(t.biggest_classes[0].pretty_class, "com.foo.A");
        assert_eq!(t.biggest_classes[1].pretty_class, "com.foo.B");
        assert_eq!(t.biggest_classes[2].pretty_class, "org.bar.C");

        // Biggest packages: "com" = obj0+obj1 = 2000 (2 objs), "org" = 200 (1 obj).
        assert_eq!(t.biggest_packages.len(), 2);
        assert_eq!(t.biggest_packages[0].package, "com");
        assert_eq!(t.biggest_packages[0].retained, 2000);
        assert_eq!(t.biggest_packages[0].objects, 2);
        assert_eq!(t.biggest_packages[1].package, "org");
        assert_eq!(t.biggest_packages[1].retained, 200);
    }

    #[test]
    fn test_build_model_leak_suspects() {
        let (g, dc_off, dc_tgt) = fixture();
        let r = build_model(&g, &dc_off, &dc_tgt);
        let l = &r.leaks;
        // total_shallow = 270, threshold = 27. Singles directly under vroot with
        // retained >= 27: obj0(1000), obj1(1000), obj3(200) all qualify.
        assert_eq!(l.total_shallow, 270);
        assert_eq!(l.suspects.len(), 3);
        // Sorted retained desc, ties by class_idx then obj_idx: obj0(class0),
        // obj1(class1) both 1000 -> class0 first; then obj3.
        assert!(l.suspects[0].is_single);
        assert_eq!(l.suspects[0].pretty_class, "com.foo.A");
        assert_eq!(l.suspects[1].pretty_class, "com.foo.B");
        assert_eq!(l.suspects[2].pretty_class, "org.bar.C");
        // Single suspect must have an accumulation path starting at itself.
        assert!(!l.suspects[0].path.is_empty());
        assert_eq!(l.suspects[0].path[0].depth, 0);
        assert_eq!(l.suspects[0].path[0].obj_index_1based, 1);
    }

    #[test]
    fn test_render_markdown_deterministic() {
        // Build the model twice and assert render output is byte-identical.
        // This specifically guards the Biggest-Packages HashMap sort fix.
        let (g1, off1, tgt1) = fixture();
        let (g2, off2, tgt2) = fixture();
        let mut r1 = build_model(&g1, &off1, &tgt1);
        let mut r2 = build_model(&g2, &off2, &tgt2);
        // Neutralise the nondeterministic timestamp line.
        r1.generated = "FIXED".to_string();
        r2.generated = "FIXED".to_string();
        assert_eq!(render_markdown(&r1), render_markdown(&r2));
    }

    #[test]
    fn test_render_markdown_structure() {
        let (g, dc_off, dc_tgt) = fixture();
        let mut r = build_model(&g, &dc_off, &dc_tgt);
        r.generated = "FIXED".to_string();
        let md = render_markdown(&r);
        assert!(md.starts_with("# Heap Dump Analysis: `test.hprof`\n\n"));
        assert!(md.contains("## System Overview\n\n"));
        assert!(md.contains("### Class Histogram (by Retained Heap)\n\n"));
        assert!(md.contains("## Leak Suspects\n\n"));
        assert!(md.contains("## Top Consumers\n\n"));
        assert!(md.contains("### Biggest Packages by Retained Heap\n\n"));
    }

    #[test]
    fn test_render_markdown_oom_triage() {
        let (g, dc_off, dc_tgt) = fixture();
        let mut r = build_model(&g, &dc_off, &dc_tgt);
        r.generated = "FIXED".to_string();
        let md = render_markdown(&r);

        // (a) new OOM-triage heading + headline retainer line present.
        assert!(
            md.contains("## OOM Triage\n\n"),
            "missing OOM Triage heading"
        );
        assert!(
            md.contains("- **Headline retainer:**"),
            "missing headline retainer line"
        );
        // Fixture's #1 suspect is com.foo.A (a single object) at 1000/270 -> dominates.
        assert!(
            md.contains("`com.foo.A`"),
            "headline should name the #1 suspect"
        );
        assert!(
            md.contains("A single object/class group dominates the heap"),
            "1000/270 is >= 50% so it should read as dominated"
        );

        // The triage block must precede System Overview.
        let tri = md.find("## OOM Triage").unwrap();
        let sys = md.find("## System Overview").unwrap();
        assert!(tri < sys, "OOM Triage must come before System Overview");

        // (b) determinism guard: render twice == identical.
        assert_eq!(md, render_markdown(&r));

        // (c) data-preservation: all key section headings still present.
        for needle in [
            "System Overview",
            "Class Histogram",
            "Leak Suspects",
            "Top Consumers",
            "Biggest Objects",
            "Biggest Classes",
            "Biggest Packages",
        ] {
            assert!(md.contains(needle), "missing section: {needle}");
        }
    }

    // ── Phase B: JSON / schema conformance ─────────────────────────────────

    /// Build the fixture Report with the nondeterministic timestamp neutralised.
    fn fixture_report() -> Report {
        let (g, dc_off, dc_tgt) = fixture();
        let mut r = build_model(&g, &dc_off, &dc_tgt);
        r.generated = "FIXED".to_string();
        r
    }

    #[test]
    fn json_round_trip() {
        let mut r = fixture_report();
        let json = serde_json::to_string(&r).expect("serialize");
        let back: Report = serde_json::from_str(&json).expect("deserialize");
        // ObjRow::pct is #[serde(skip)] (f64 kept out of JSON), so it
        // deserializes to its Default (0.0). Zero it on the original before
        // comparing; every OTHER field must survive the round trip.
        for row in &mut r.top.biggest_objects {
            row.pct = 0.0;
        }
        assert_eq!(r, back, "round-tripped Report must equal the original");
    }

    #[test]
    fn render_markdown_round_trips_through_json() {
        // Proves the --render offline path is faithful: serializing a Report to
        // JSON and deserializing it back must produce byte-identical Markdown.
        let r = fixture_report();
        let json = serde_json::to_string(&r).expect("serialize");
        let back: Report = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(
            render_markdown(&r),
            render_markdown(&back),
            "render_markdown must be stable across a JSON round trip"
        );
    }

    #[test]
    fn json_serialization_is_deterministic() {
        let r = fixture_report();
        let a = serde_json::to_string_pretty(&r).unwrap();
        let b = serde_json::to_string_pretty(&r).unwrap();
        assert_eq!(
            a, b,
            "serializing the same Report twice must be byte-identical"
        );
    }

    #[test]
    fn json_validates_against_schema() {
        let r = fixture_report();
        let instance = serde_json::to_value(&r).expect("Report -> Value");
        let schema = serde_json::to_value(schemars::schema_for!(Report)).expect("schema -> Value");
        let validator = jsonschema::validator_for(&schema).expect("compile schema (draft 2020-12)");
        assert!(
            validator.validate(&instance).is_ok(),
            "serialized fixture Report must validate against schema_for!(Report)"
        );
    }

    #[test]
    fn emit_schema_matches_committed_file() {
        let committed: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/schema/report.schema.json"
            ))
            .expect("read committed schema"),
        )
        .expect("parse committed schema");
        let fresh = serde_json::to_value(schemars::schema_for!(Report)).expect("fresh schema");
        // Value-equality: whitespace / key ordering must not cause false diffs.
        assert_eq!(
            committed, fresh,
            "schema/report.schema.json must equal a fresh schema_for!(Report);              regenerate via `--emit-schema` if the model changed"
        );
    }

    #[test]
    fn schema_version_guard() {
        let r = fixture_report();
        assert_eq!(r.schema_version, SCHEMA_VERSION);
        assert_eq!(SCHEMA_VERSION, 1);
    }
}

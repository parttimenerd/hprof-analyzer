//! B9: cross-dump growth diff.
//!
//! A pure offline post-processing tool. It reads TWO canonical `Report` JSON
//! files (produced by `hprof-analyzer analyze <dump> --format json`), joins
//! their class histograms by class name, and reports how the heap GREW between
//! the baseline (A) and the current (B) dump of the same application.
//!
//! It never parses a heap dump or touches the analysis pipeline; it only reads
//! two small JSON models, diffs them, and renders a report. Output is fully
//! deterministic: every list is sorted by an explicit key with a stable
//! tie-breaker, and the JSON result carries only integers (no f64).

use std::collections::BTreeMap;
use std::io::{self, Read};

use crate::OutputFormat;
use crate::md::{Align, Table};
use crate::report::{self, Report};

/// Cap on the number of rows shown in the growth-leaders and new-classes
/// lists. Suspects are uncapped (there are only ever a handful).
const TOP_N: usize = 25;

/// One joined class row: how instances/retained changed from A to B.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct ClassDelta {
    pub pretty_class: String,
    pub delta_instances: i64,
    pub delta_retained: i64,
    pub a_retained: u64,
    pub b_retained: u64,
}

/// One joined leak-suspect row: how a suspect's retained heap changed, or that
/// it is entirely new in B.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct SuspectDelta {
    pub pretty_class: String,
    pub a_retained: u64,
    pub b_retained: u64,
    pub delta_retained: i64,
    pub is_new: bool,
}

/// The machine-readable cross-dump diff. INTERNAL to this module — it is not
/// part of the committed `Report` model or JSON schema.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct DiffReportsResult {
    pub delta_total_objects: i64,
    pub delta_total_shallow: i64,
    pub net_delta_retained: i64,
    pub growth_leaders: Vec<ClassDelta>,
    pub new_classes: Vec<ClassDelta>,
    pub grown_suspects: Vec<SuspectDelta>,
}

/// Load a canonical `Report` from a path (or stdin for "-"), rejecting a
/// schema-version mismatch. Mirrors `render_report` in main.rs.
fn load_report(path: &str) -> io::Result<Report> {
    let json = if path == "-" {
        let mut buf = String::new();
        io::stdin().read_to_string(&mut buf)?;
        buf
    } else {
        std::fs::read_to_string(path)?
    };
    let report: Report = serde_json::from_str(&json).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid report JSON ({path}): {e}"),
        )
    })?;
    if report.schema_version != report::SCHEMA_VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "report {} schema_version {} does not match supported version {}; refusing to diff",
                path,
                report.schema_version,
                report::SCHEMA_VERSION
            ),
        ));
    }
    Ok(report)
}

/// Compute the cross-dump growth diff. Pure and testable: joins A's and B's
/// class histograms by name, then their leak suspects by `pretty_class`.
pub fn diff(a: &Report, b: &Report) -> DiffReportsResult {
    // Join histograms by class name. Value = (a_row, b_row); either may be
    // absent. Using a BTreeMap gives deterministic iteration (name-sorted),
    // so no HashMap ordering ever leaks into the output.
    #[derive(Default, Clone, Copy)]
    struct Pair {
        a_inst: u64,
        a_ret: u64,
        a_present: bool,
        b_inst: u64,
        b_ret: u64,
        b_present: bool,
    }
    let mut joined: BTreeMap<&str, Pair> = BTreeMap::new();
    for row in &a.overview.histogram {
        let e = joined.entry(row.pretty_class.as_str()).or_default();
        e.a_inst = row.instances;
        e.a_ret = row.retained;
        e.a_present = true;
    }
    for row in &b.overview.histogram {
        let e = joined.entry(row.pretty_class.as_str()).or_default();
        e.b_inst = row.instances;
        e.b_ret = row.retained;
        e.b_present = true;
    }

    let mut all_deltas: Vec<ClassDelta> = Vec::with_capacity(joined.len());
    let mut new_classes: Vec<ClassDelta> = Vec::new();
    let mut net_delta_retained: i64 = 0;
    for (name, p) in &joined {
        let delta_instances = p.b_inst as i64 - p.a_inst as i64;
        let delta_retained = p.b_ret as i64 - p.a_ret as i64;
        net_delta_retained += delta_retained;
        let cd = ClassDelta {
            pretty_class: (*name).to_string(),
            delta_instances,
            delta_retained,
            a_retained: p.a_ret,
            b_retained: p.b_ret,
        };
        // A class is "new" iff it is present in B and absent from A.
        if p.b_present && !p.a_present {
            new_classes.push(cd.clone());
        }
        all_deltas.push(cd);
    }

    // Growth leaders: classes with the largest POSITIVE Δretained, descending.
    // Non-positive deltas are excluded (they did not grow). Tie-break on class
    // name ascending for a stable, deterministic order.
    let mut growth_leaders: Vec<ClassDelta> = all_deltas
        .into_iter()
        .filter(|c| c.delta_retained > 0)
        .collect();
    growth_leaders.sort_by(|x, y| {
        y.delta_retained
            .cmp(&x.delta_retained)
            .then_with(|| x.pretty_class.cmp(&y.pretty_class))
    });
    growth_leaders.truncate(TOP_N);

    // New classes: sorted by B.retained descending, then name ascending.
    new_classes.sort_by(|x, y| {
        y.b_retained
            .cmp(&x.b_retained)
            .then_with(|| x.pretty_class.cmp(&y.pretty_class))
    });
    new_classes.truncate(TOP_N);

    // Suspects: join by pretty_class. Report suspects that are new in B or whose
    // retained grew vs the same-named suspect in A. If a report has duplicate
    // suspect names (rare), keep the max retained as that side's value for a
    // conservative "grew" test.
    let mut a_best: BTreeMap<&str, u64> = BTreeMap::new();
    for s in &a.leaks.suspects {
        let e = a_best.entry(s.pretty_class.as_str()).or_insert(0);
        *e = (*e).max(s.retained);
    }
    let mut b_best: BTreeMap<&str, u64> = BTreeMap::new();
    for s in &b.leaks.suspects {
        let e = b_best.entry(s.pretty_class.as_str()).or_insert(0);
        *e = (*e).max(s.retained);
    }
    let mut grown_suspects: Vec<SuspectDelta> = Vec::new();
    for (name, &b_ret) in &b_best {
        let a_ret_opt = a_best.get(name).copied();
        let is_new = a_ret_opt.is_none();
        let a_ret = a_ret_opt.unwrap_or(0);
        let delta_retained = b_ret as i64 - a_ret as i64;
        // Keep only suspects that are new in B or whose retained grew.
        if is_new || delta_retained > 0 {
            grown_suspects.push(SuspectDelta {
                pretty_class: (*name).to_string(),
                a_retained: a_ret,
                b_retained: b_ret,
                delta_retained,
                is_new,
            });
        }
    }
    grown_suspects.sort_by(|x, y| {
        y.b_retained
            .cmp(&x.b_retained)
            .then_with(|| x.pretty_class.cmp(&y.pretty_class))
    });

    DiffReportsResult {
        delta_total_objects: b.overview.total_objects as i64 - a.overview.total_objects as i64,
        delta_total_shallow: b.overview.total_shallow as i64 - a.overview.total_shallow as i64,
        net_delta_retained,
        growth_leaders,
        new_classes,
        grown_suspects,
    }
}

// ── Formatting helpers ─────────────────────────────────────────────────────

/// Unicode minus sign (U+2212), matching the report style's preference for a
/// typographic minus over a hyphen for negative values.
const MINUS: char = '\u{2212}';

/// Format a signed byte delta as e.g. "+1.2 MB" / "\u{2212}340 KB" / "0 B".
fn fmt_delta_bytes(n: i64) -> String {
    if n == 0 {
        return "0 B".to_string();
    }
    let sign = if n > 0 { '+' } else { MINUS };
    let mag = report::format_bytes(n.unsigned_abs());
    format!("{sign}{mag}")
}

/// Format a signed instance-count delta with thousands separators, e.g.
/// "+1,024" / "\u{2212}17" / "0".
fn fmt_delta_count(n: i64) -> String {
    if n == 0 {
        return "0".to_string();
    }
    let sign = if n > 0 { '+' } else { MINUS };
    let s = n.unsigned_abs().to_string();
    let mut grouped = String::new();
    for (i, c) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(c);
    }
    let grouped: String = grouped.chars().rev().collect();
    format!("{sign}{grouped}")
}

/// Render the diff as human-readable Markdown.
pub fn render_md(d: &DiffReportsResult) -> String {
    let mut out = String::new();
    out.push_str("## Cross-Dump Growth\n\n");
    out.push_str(
        "_How the reachable heap grew from the baseline (A) to the current (B) dump._\n\n",
    );

    out.push_str("### Headline Totals\n\n");
    out.push_str(&format!(
        "- **Δ Objects:** {}\n",
        fmt_delta_count(d.delta_total_objects)
    ));
    out.push_str(&format!(
        "- **Δ Shallow heap:** {}\n",
        fmt_delta_bytes(d.delta_total_shallow)
    ));
    out.push_str(&format!(
        "- **Net Δ Retained (all classes):** {}\n\n",
        fmt_delta_bytes(d.net_delta_retained)
    ));

    out.push_str("### Growth Leaders (by Δ retained)\n\n");
    if d.growth_leaders.is_empty() {
        out.push_str("No class grew in retained heap.\n\n");
    } else {
        let mut t = Table::new(
            &["Class", "Δ Instances", "Δ Retained", "Retained (B)"],
            &[Align::Left, Align::Right, Align::Right, Align::Right],
        );
        for c in &d.growth_leaders {
            t.row([
                format!("`{}`", c.pretty_class),
                fmt_delta_count(c.delta_instances),
                fmt_delta_bytes(c.delta_retained),
                report::format_bytes(c.b_retained),
            ]);
        }
        t.render(&mut out);
        out.push('\n');
    }

    out.push_str("### New Classes\n\n");
    if d.new_classes.is_empty() {
        out.push_str("No classes are new in the current dump.\n\n");
    } else {
        let mut t = Table::new(
            &["Class", "Instances (B)", "Retained (B)"],
            &[Align::Left, Align::Right, Align::Right],
        );
        for c in &d.new_classes {
            t.row([
                format!("`{}`", c.pretty_class),
                fmt_delta_count(c.delta_instances),
                report::format_bytes(c.b_retained),
            ]);
        }
        t.render(&mut out);
        out.push('\n');
    }

    out.push_str("### New / Grown Leak Suspects\n\n");
    if d.grown_suspects.is_empty() {
        out.push_str("No leak suspect is new or grew in the current dump.\n\n");
    } else {
        let mut t = Table::new(
            &[
                "Suspect",
                "Retained (A)",
                "Retained (B)",
                "Δ Retained",
                "New?",
            ],
            &[
                Align::Left,
                Align::Right,
                Align::Right,
                Align::Right,
                Align::Left,
            ],
        );
        for s in &d.grown_suspects {
            t.row([
                format!("`{}`", s.pretty_class),
                report::format_bytes(s.a_retained),
                report::format_bytes(s.b_retained),
                fmt_delta_bytes(s.delta_retained),
                if s.is_new {
                    "yes".to_string()
                } else {
                    String::new()
                },
            ]);
        }
        t.render(&mut out);
        out.push('\n');
    }

    out
}

/// Thin wrapper: load both reports (version-checked), diff, and render.
pub fn run(a_path: &str, b_path: &str, format: OutputFormat) -> io::Result<String> {
    let a = load_report(a_path)?;
    let b = load_report(b_path)?;
    let result = diff(&a, &b);
    Ok(match format {
        OutputFormat::Json => serde_json::to_string_pretty(&result).map_err(io::Error::other)?,
        // The cross-dump diff has no HTML/graphics view; fall back to Markdown.
        OutputFormat::Md | OutputFormat::MdGraphs | OutputFormat::Html => render_md(&result),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::{
        HistRow, LeakSuspects, PackageNode, SCHEMA_VERSION, Suspect, SystemOverview, TopConsumers,
    };

    fn hist(name: &str, inst: u64, sh: u64, ret: u64) -> HistRow {
        HistRow {
            pretty_class: name.to_string(),
            instances: inst,
            shallow: sh,
            retained: ret,
            loader_id: 0,
            loader_label: None,
        }
    }

    fn suspect(name: &str, inst: u64, ret: u64) -> Suspect {
        Suspect {
            is_single: false,
            pretty_class: name.to_string(),
            instance_count: inst,
            retained: ret,
            shallow: 0,
            path: vec![],
            accumulation_obj_1based: None,
            accumulation_class: None,
            accumulation_retained: None,
            dominated: vec![],
            dominated_total_count: 0,
            dominated_shown: 0,
            dominated_by_class: vec![],
            keywords: vec![],
            root_type_label: String::new(),
        }
    }

    fn base_report(
        total_objects: u64,
        total_shallow: u64,
        histogram: Vec<HistRow>,
        suspects: Vec<Suspect>,
    ) -> Report {
        Report {
            schema_version: SCHEMA_VERSION,
            generated: "x".to_string(),
            overview: SystemOverview {
                source_name: "s".to_string(),
                file_path: "s".to_string(),
                format: "hprof".to_string(),
                file_size: 100,
                identifier_size_bits: 64,
                compressed_oops: None,
                dump_creation: None,
                total_objects,
                total_shallow,
                gc_roots: 5,
                gc_roots_by_type: vec![],
                heap_composition: Default::default(),
                dominator_depth_histogram: vec![],
                retention_concentration: Default::default(),
                classes_loaded: 3,
                classloaders_loaded: 1,
                unreachable_count: 0,
                unreachable_shallow: 0,
                histogram,
                histogram_truncated_to: None,
                system_properties: vec![],
                jvm_version: None,
                loader_rollup: vec![],
                duplicate_classes: vec![],
            },
            leaks: LeakSuspects {
                total_shallow,
                suspects,
            },
            top: TopConsumers {
                biggest_objects: vec![],
                biggest_classes: vec![],
                threshold_bp: 100,
                biggest_packages: PackageNode {
                    name: String::new(),
                    top_dominator_count: 0,
                    shallow_heap: 0,
                    retained_heap: 0,
                    children: vec![],
                },
            },
            threads: crate::report::ThreadOverview { threads: vec![] },
        }
    }

    #[test]
    fn class_delta_join_grew_shrank_new_removed() {
        // A: grew (100->300), shrank (500->200), removed (present in A only).
        let a = base_report(
            0,
            0,
            vec![
                hist("Grew", 1, 10, 100),
                hist("Shrank", 5, 50, 500),
                hist("Removed", 2, 20, 200),
            ],
            vec![],
        );
        // B: grew, shrank, new (present in B only).
        let b = base_report(
            0,
            0,
            vec![
                hist("Grew", 3, 30, 300),
                hist("Shrank", 2, 20, 200),
                hist("NewClass", 4, 40, 400),
            ],
            vec![],
        );
        let d = diff(&a, &b);

        // Growth leaders: only positive Δretained. Grew (+200) and NewClass
        // (+400, since A absent = 0). Sorted desc: NewClass then Grew.
        assert_eq!(d.growth_leaders.len(), 2);
        assert_eq!(d.growth_leaders[0].pretty_class, "NewClass");
        assert_eq!(d.growth_leaders[0].delta_retained, 400);
        assert_eq!(d.growth_leaders[1].pretty_class, "Grew");
        assert_eq!(d.growth_leaders[1].delta_retained, 200);
        assert_eq!(d.growth_leaders[1].delta_instances, 2);
        // Shrank (-300) and Removed (-200) must NOT appear.
        assert!(
            !d.growth_leaders
                .iter()
                .any(|c| c.pretty_class == "Shrank" || c.pretty_class == "Removed")
        );

        // Sorted strictly descending by delta_retained.
        for w in d.growth_leaders.windows(2) {
            assert!(w[0].delta_retained >= w[1].delta_retained);
        }
    }

    #[test]
    fn new_class_appears_once() {
        let a = base_report(0, 0, vec![hist("Old", 1, 10, 100)], vec![]);
        let b = base_report(
            0,
            0,
            vec![hist("Old", 1, 10, 100), hist("Fresh", 7, 70, 700)],
            vec![],
        );
        let d = diff(&a, &b);
        assert_eq!(d.new_classes.len(), 1);
        assert_eq!(d.new_classes[0].pretty_class, "Fresh");
        assert_eq!(d.new_classes[0].b_retained, 700);
        // 0 -> 7 instances.
        assert_eq!(d.new_classes[0].delta_instances, 7);
        assert_eq!(d.new_classes[0].a_retained, 0);
        // "Old" (unchanged) is not new.
        assert!(!d.new_classes.iter().any(|c| c.pretty_class == "Old"));
    }

    #[test]
    fn suspect_delta_new_and_grown_sorted() {
        let a = base_report(
            0,
            0,
            vec![],
            vec![
                suspect("GrownSuspect", 10, 1_000),
                suspect("Stable", 5, 500),
            ],
        );
        let b = base_report(
            0,
            0,
            vec![],
            vec![
                suspect("GrownSuspect", 20, 3_000), // grew +2000
                suspect("Stable", 5, 500),          // unchanged -> excluded
                suspect("BrandNew", 8, 5_000),      // new
            ],
        );
        let d = diff(&a, &b);
        // BrandNew (5000, new) and GrownSuspect (3000, grew); Stable excluded.
        assert_eq!(d.grown_suspects.len(), 2);
        // Sorted by b_retained desc: BrandNew (5000) then GrownSuspect (3000).
        assert_eq!(d.grown_suspects[0].pretty_class, "BrandNew");
        assert!(d.grown_suspects[0].is_new);
        assert_eq!(d.grown_suspects[0].a_retained, 0);
        assert_eq!(d.grown_suspects[1].pretty_class, "GrownSuspect");
        assert!(!d.grown_suspects[1].is_new);
        assert_eq!(d.grown_suspects[1].delta_retained, 2_000);
        assert!(!d.grown_suspects.iter().any(|s| s.pretty_class == "Stable"));
    }

    #[test]
    fn headline_totals_including_negative() {
        // B smaller objects, larger shallow, mixed retained.
        let a = base_report(
            100,
            5_000,
            vec![hist("A", 1, 10, 1_000), hist("B", 1, 10, 2_000)],
            vec![],
        );
        let b = base_report(
            80, // objects shrank
            7_500,
            vec![hist("A", 1, 10, 500), hist("B", 1, 10, 4_000)],
            vec![],
        );
        let d = diff(&a, &b);
        assert_eq!(d.delta_total_objects, -20);
        assert_eq!(d.delta_total_shallow, 2_500);
        // net retained: A -500, B +2000 => +1500.
        assert_eq!(d.net_delta_retained, 1_500);
    }

    #[test]
    fn schema_version_mismatch_is_err() {
        let mut a = base_report(0, 0, vec![], vec![]);
        a.schema_version = SCHEMA_VERSION + 1;
        let json = serde_json::to_string(&a).unwrap();
        let dir = std::env::temp_dir();
        let path = dir.join(format!(
            "diff_reports_bad_schema_{}.json",
            std::process::id()
        ));
        std::fs::write(&path, json).unwrap();
        let res = load_report(path.to_str().unwrap());
        let _ = std::fs::remove_file(&path);
        assert!(res.is_err());
        let e = res.err().unwrap();
        assert_eq!(e.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn render_md_smoke() {
        let a = base_report(0, 0, vec![hist("Foo", 1, 10, 100)], vec![]);
        let b = base_report(
            0,
            0,
            vec![hist("Foo", 5, 50, 900)],
            vec![suspect("com.example.Leaky", 3, 9_999)],
        );
        let d = diff(&a, &b);
        let md = render_md(&d);
        assert!(md.contains("## Cross-Dump Growth"));
        assert!(md.contains("### Headline Totals"));
        assert!(md.contains("### Growth Leaders (by Δ retained)"));
        assert!(md.contains("### New Classes"));
        assert!(md.contains("### New / Grown Leak Suspects"));
        // The grown class row is present.
        assert!(md.contains("`Foo`"));
        // The new suspect row is present.
        assert!(md.contains("`com.example.Leaky`"));
    }

    #[test]
    fn all_same_is_all_zero() {
        let a = base_report(
            10,
            1_000,
            vec![hist("X", 2, 20, 200)],
            vec![suspect("S", 1, 100)],
        );
        let d = diff(&a, &a);
        assert_eq!(d.delta_total_objects, 0);
        assert_eq!(d.delta_total_shallow, 0);
        assert_eq!(d.net_delta_retained, 0);
        assert!(d.growth_leaders.is_empty());
        assert!(d.new_classes.is_empty());
        assert!(d.grown_suspects.is_empty());
    }
}

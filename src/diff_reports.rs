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

// ── N-way time-series diff ──────────────────────────────────────────────────

/// One joined class row across N reports: its instances/retained at each report
/// (index 0 = first, N−1 = last), plus the first→last deltas.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct SeriesClassRow {
    pub pretty_class: String,
    /// Retained heap per report, len N. `0` where the class is absent.
    pub retained: Vec<u64>,
    /// Instance count per report, len N. `0` where the class is absent.
    pub instances: Vec<u64>,
    /// last − first retained.
    pub delta_retained: i64,
    /// last − first instances.
    pub delta_instances: i64,
}

/// One joined leak-suspect row across N reports.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct SeriesSuspectRow {
    pub pretty_class: String,
    /// Retained heap per report, len N. `0` where absent in that report.
    pub retained: Vec<u64>,
    /// last − first retained.
    pub delta_retained: i64,
    /// Absent in the first report, present in the last.
    pub is_new: bool,
    /// Present in the first report, absent in the last.
    pub is_gone: bool,
}

/// The machine-readable N-way cross-dump time-series diff. INTERNAL to this
/// module — not part of the committed `Report` model or JSON schema. Every
/// value is an integer; every list is deterministically sorted.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct SeriesDiffResult {
    /// One label per report, in input order (from `source_name`, len N).
    pub labels: Vec<String>,
    /// Total reachable objects per report, len N.
    pub total_objects: Vec<u64>,
    /// Total shallow heap per report, len N.
    pub total_shallow: Vec<u64>,
    /// last − first total objects.
    pub delta_total_objects: i64,
    /// last − first total shallow.
    pub delta_total_shallow: i64,
    /// Sum over classes of (last − first) retained.
    pub net_delta_retained: i64,
    pub growth_leaders: Vec<SeriesClassRow>,
    /// Classes absent in the first report, present in the last.
    pub new_classes: Vec<SeriesClassRow>,
    /// Classes present in the first report, absent in the last.
    pub removed_classes: Vec<SeriesClassRow>,
    /// Suspects new-in-last OR grown first→last.
    pub grown_suspects: Vec<SeriesSuspectRow>,
    /// Suspects present at both ends whose retained fell.
    pub shrunk_suspects: Vec<SeriesSuspectRow>,
    /// Suspects present in the first report, absent in the last.
    pub gone_suspects: Vec<SeriesSuspectRow>,
}

/// Compute the N-way cross-dump time-series diff. Pure and deterministic:
/// joins ALL reports' histograms and leak suspects by `pretty_class` via
/// `BTreeMap` (name-sorted iteration), then builds each list sorted by an
/// explicit key with a `pretty_class` tie-break. "first" = report 0, "last" =
/// report N−1. When N == 2 the first→last numbers match the pairwise `diff`.
pub fn diff_series(reports: &[Report]) -> SeriesDiffResult {
    let n = reports.len();
    let last = n.saturating_sub(1);

    // Labels: source_name, falling back to a 1-based positional name.
    let labels: Vec<String> = reports
        .iter()
        .enumerate()
        .map(|(i, r)| {
            if r.overview.source_name.is_empty() {
                format!("report {}", i + 1)
            } else {
                r.overview.source_name.clone()
            }
        })
        .collect();

    let total_objects: Vec<u64> = reports.iter().map(|r| r.overview.total_objects).collect();
    let total_shallow: Vec<u64> = reports.iter().map(|r| r.overview.total_shallow).collect();
    let delta_total_objects = if n == 0 {
        0
    } else {
        total_objects[last] as i64 - total_objects[0] as i64
    };
    let delta_total_shallow = if n == 0 {
        0
    } else {
        total_shallow[last] as i64 - total_shallow[0] as i64
    };

    // Join histograms by class name across all N reports. Each value is a len-N
    // vector of Option<(instances, retained)>; None = class absent in that report.
    let mut joined: BTreeMap<&str, Vec<Option<(u64, u64)>>> = BTreeMap::new();
    for (i, r) in reports.iter().enumerate() {
        for row in &r.overview.histogram {
            let e = joined
                .entry(row.pretty_class.as_str())
                .or_insert_with(|| vec![None; n]);
            e[i] = Some((row.instances, row.retained));
        }
    }

    let mut all_rows: Vec<SeriesClassRow> = Vec::with_capacity(joined.len());
    let mut new_classes: Vec<SeriesClassRow> = Vec::new();
    let mut removed_classes: Vec<SeriesClassRow> = Vec::new();
    let mut net_delta_retained: i64 = 0;
    for (name, cells) in &joined {
        let instances: Vec<u64> = cells.iter().map(|c| c.map_or(0, |(i, _)| i)).collect();
        let retained: Vec<u64> = cells.iter().map(|c| c.map_or(0, |(_, r)| r)).collect();
        let first_present = n > 0 && cells[0].is_some();
        let last_present = n > 0 && cells[last].is_some();
        let delta_retained = if n == 0 {
            0
        } else {
            retained[last] as i64 - retained[0] as i64
        };
        let delta_instances = if n == 0 {
            0
        } else {
            instances[last] as i64 - instances[0] as i64
        };
        net_delta_retained += delta_retained;
        let row = SeriesClassRow {
            pretty_class: (*name).to_string(),
            retained,
            instances,
            delta_retained,
            delta_instances,
        };
        // "new" iff present in last and absent in first; "removed" iff reverse.
        if last_present && !first_present {
            new_classes.push(row.clone());
        } else if first_present && !last_present {
            removed_classes.push(row.clone());
        }
        all_rows.push(row);
    }

    // Growth leaders: largest POSITIVE first→last Δretained, desc, name tie-break.
    let mut growth_leaders: Vec<SeriesClassRow> = all_rows
        .into_iter()
        .filter(|c| c.delta_retained > 0)
        .collect();
    growth_leaders.sort_by(|x, y| {
        y.delta_retained
            .cmp(&x.delta_retained)
            .then_with(|| x.pretty_class.cmp(&y.pretty_class))
    });
    growth_leaders.truncate(TOP_N);

    // New classes: sorted by last retained desc, then name asc.
    new_classes.sort_by(|x, y| {
        y.retained[last]
            .cmp(&x.retained[last])
            .then_with(|| x.pretty_class.cmp(&y.pretty_class))
    });
    new_classes.truncate(TOP_N);

    // Removed classes: sorted by first retained desc, then name asc.
    removed_classes.sort_by(|x, y| {
        y.retained[0]
            .cmp(&x.retained[0])
            .then_with(|| x.pretty_class.cmp(&y.pretty_class))
    });
    removed_classes.truncate(TOP_N);

    // Suspects: join by pretty_class across all N reports, keeping the MAX
    // retained where a report has duplicate suspect names (as pairwise does).
    let mut suspects: BTreeMap<&str, Vec<Option<u64>>> = BTreeMap::new();
    for (i, r) in reports.iter().enumerate() {
        for s in &r.leaks.suspects {
            let e = suspects
                .entry(s.pretty_class.as_str())
                .or_insert_with(|| vec![None; n]);
            e[i] = Some(e[i].map_or(s.retained, |cur| cur.max(s.retained)));
        }
    }

    let mut grown_suspects: Vec<SeriesSuspectRow> = Vec::new();
    let mut shrunk_suspects: Vec<SeriesSuspectRow> = Vec::new();
    let mut gone_suspects: Vec<SeriesSuspectRow> = Vec::new();
    for (name, cells) in &suspects {
        let retained: Vec<u64> = cells.iter().map(|c| c.unwrap_or(0)).collect();
        let first_present = n > 0 && cells[0].is_some();
        let last_present = n > 0 && cells[last].is_some();
        let delta_retained = if n == 0 {
            0
        } else {
            retained[last] as i64 - retained[0] as i64
        };
        let is_new = last_present && !first_present;
        let is_gone = first_present && !last_present;
        let row = SeriesSuspectRow {
            pretty_class: (*name).to_string(),
            retained,
            delta_retained,
            is_new,
            is_gone,
        };
        if is_gone {
            gone_suspects.push(row);
        } else if is_new || delta_retained > 0 {
            // Only classify present-in-last suspects into grown; gone handled above.
            if last_present {
                grown_suspects.push(row);
            }
        } else if last_present && first_present && delta_retained < 0 {
            shrunk_suspects.push(row);
        }
    }
    // Grown: by last retained desc, then name asc.
    grown_suspects.sort_by(|x, y| {
        y.retained[last]
            .cmp(&x.retained[last])
            .then_with(|| x.pretty_class.cmp(&y.pretty_class))
    });
    // Shrunk: most-negative delta first, then name asc.
    shrunk_suspects.sort_by(|x, y| {
        x.delta_retained
            .cmp(&y.delta_retained)
            .then_with(|| x.pretty_class.cmp(&y.pretty_class))
    });
    // Gone: by first retained desc, then name asc.
    gone_suspects.sort_by(|x, y| {
        y.retained[0]
            .cmp(&x.retained[0])
            .then_with(|| x.pretty_class.cmp(&y.pretty_class))
    });

    SeriesDiffResult {
        labels,
        total_objects,
        total_shallow,
        delta_total_objects,
        delta_total_shallow,
        net_delta_retained,
        growth_leaders,
        new_classes,
        removed_classes,
        grown_suspects,
        shrunk_suspects,
        gone_suspects,
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

/// A one-line, plain-language verdict for the top of the cross-dump report.
/// The grew/shrank magnitude uses the change in *total shallow heap* (the real,
/// bounded heap size) from the first to the last report as its basis — net
/// retained sums per-class retention, which overlaps and can exceed the heap,
/// so it is unsuitable as a percentage denominator. The denominator is the
/// FIRST report's total shallow heap. The largest *retained* driver is still
/// named, as it best explains the change. Pure and deterministic — integers and
/// a single rounded percentage only (the only f64 in the whole renderer).
fn verdict(d: &SeriesDiffResult) -> String {
    let first_shallow = d.total_shallow.first().copied().unwrap_or(0);
    let pct = if first_shallow > 0 {
        d.delta_total_shallow as f64 / first_shallow as f64 * 100.0
    } else {
        0.0
    };
    let new_suspects = d.grown_suspects.iter().filter(|s| s.is_new).count();
    let mut line = if d.delta_total_shallow > 0 {
        let driver = d
            .growth_leaders
            .first()
            .map(|c| {
                format!(
                    "; largest driver `{}` ({} retained)",
                    c.pretty_class,
                    fmt_delta_bytes(c.delta_retained)
                )
            })
            .unwrap_or_default();
        format!(
            "Heap grew {:.1}% ({} shallow){}.",
            pct,
            fmt_delta_bytes(d.delta_total_shallow),
            driver,
        )
    } else if d.delta_total_shallow < 0 {
        format!(
            "Heap shrank {:.1}% ({} shallow); no net growth.",
            pct.abs(),
            fmt_delta_bytes(d.delta_total_shallow),
        )
    } else {
        "Heap size is unchanged.".to_string()
    };
    if new_suspects > 0 {
        let plural = if new_suspects == 1 { "" } else { "s" };
        line.push_str(&format!(" {new_suspects} new suspect{plural}."));
    }
    line
}

/// The per-report column headers `r1`..`rN` for the N value columns.
fn report_col_headers(n: usize) -> Vec<String> {
    (1..=n).map(|i| format!("r{i}")).collect()
}

/// Build a per-report `Table` for a class/suspect section: `label | r1 … rN |
/// Δ(first→last)`. The first column is left-aligned; every value/Δ column is
/// right-aligned. `n` is the report count.
fn series_table(label: &str, n: usize) -> Table {
    let mut headers: Vec<String> = vec![label.to_string()];
    headers.extend(report_col_headers(n));
    headers.push("Δ(r1→rN)".to_string());
    let mut aligns: Vec<Align> = vec![Align::Left];
    aligns.extend(std::iter::repeat_n(Align::Right, n + 1));
    let header_refs: Vec<&str> = headers.iter().map(String::as_str).collect();
    Table::new(&header_refs, &aligns)
}

/// One row of retained bytes across N reports plus a signed Δ column.
fn retained_row(pretty_class: &str, retained: &[u64], delta_retained: i64) -> Vec<String> {
    let mut cells: Vec<String> = vec![format!("`{pretty_class}`")];
    for &r in retained {
        cells.push(report::format_bytes(r));
    }
    cells.push(fmt_delta_bytes(delta_retained));
    cells
}

/// Render the N-way time-series diff as human-readable Markdown.
pub fn render_md(d: &SeriesDiffResult) -> String {
    let n = d.labels.len();
    let mut out = String::new();
    out.push_str("## Cross-Dump Growth\n\n");
    out.push_str(
        "_How the reachable heap grew across a time series of dumps of the same application \
         (first = baseline, last = current)._\n\n",
    );

    // Legend: map each rN column to its report label so the tables are readable.
    out.push_str("### Reports\n\n");
    for (i, label) in d.labels.iter().enumerate() {
        out.push_str(&format!("- `r{}` = {}\n", i + 1, label));
    }
    out.push('\n');

    out.push_str(&format!("**Verdict:** {}\n\n", verdict(d)));

    out.push_str("### Headline Totals\n\n");
    out.push_str(&format!(
        "- **Δ Objects (r1→rN):** {}\n",
        fmt_delta_count(d.delta_total_objects)
    ));
    out.push_str(&format!(
        "- **Δ Shallow heap (r1→rN):** {}\n",
        fmt_delta_bytes(d.delta_total_shallow)
    ));
    out.push_str(&format!(
        "- **Net Δ Retained (all classes, r1→rN):** {}\n\n",
        fmt_delta_bytes(d.net_delta_retained)
    ));

    out.push_str("### Growth Leaders (by Δ retained)\n\n");
    if d.growth_leaders.is_empty() {
        out.push_str("No class grew in retained heap.\n\n");
    } else {
        let mut t = series_table("Class", n);
        for c in &d.growth_leaders {
            t.row(retained_row(&c.pretty_class, &c.retained, c.delta_retained));
        }
        t.render(&mut out);
        out.push('\n');
    }

    out.push_str("### New Classes\n\n");
    if d.new_classes.is_empty() {
        out.push_str("No classes are new in the current dump.\n\n");
    } else {
        let mut t = series_table("Class", n);
        for c in &d.new_classes {
            t.row(retained_row(&c.pretty_class, &c.retained, c.delta_retained));
        }
        t.render(&mut out);
        out.push('\n');
    }

    out.push_str("### Removed Classes\n\n");
    if d.removed_classes.is_empty() {
        out.push_str("No classes dropped out of the current dump.\n\n");
    } else {
        let mut t = series_table("Class", n);
        for c in &d.removed_classes {
            t.row(retained_row(&c.pretty_class, &c.retained, c.delta_retained));
        }
        t.render(&mut out);
        out.push('\n');
    }

    out.push_str("### New / Grown Leak Suspects\n\n");
    if d.grown_suspects.is_empty() {
        out.push_str("No leak suspect is new or grew in the current dump.\n\n");
    } else {
        // Suspects add a trailing "New?" flag column after the Δ column.
        let mut headers: Vec<String> = vec!["Suspect".to_string()];
        headers.extend(report_col_headers(n));
        headers.push("Δ(r1→rN)".to_string());
        headers.push("New?".to_string());
        let mut aligns: Vec<Align> = vec![Align::Left];
        aligns.extend(std::iter::repeat_n(Align::Right, n + 1));
        aligns.push(Align::Left);
        let header_refs: Vec<&str> = headers.iter().map(String::as_str).collect();
        let mut t = Table::new(&header_refs, &aligns);
        for s in &d.grown_suspects {
            let mut cells = retained_row(&s.pretty_class, &s.retained, s.delta_retained);
            cells.push(if s.is_new {
                "yes".to_string()
            } else {
                String::new()
            });
            t.row(cells);
        }
        t.render(&mut out);
        out.push('\n');
    }

    out.push_str("### Shrunk Leak Suspects\n\n");
    if d.shrunk_suspects.is_empty() {
        out.push_str("No leak suspect shrank in the current dump.\n\n");
    } else {
        let mut t = series_table("Suspect", n);
        for s in &d.shrunk_suspects {
            t.row(retained_row(&s.pretty_class, &s.retained, s.delta_retained));
        }
        t.render(&mut out);
        out.push('\n');
    }

    out.push_str("### Disappeared Leak Suspects\n\n");
    if d.gone_suspects.is_empty() {
        out.push_str("No leak suspect disappeared in the current dump.\n\n");
    } else {
        let mut t = series_table("Suspect", n);
        for s in &d.gone_suspects {
            t.row(retained_row(&s.pretty_class, &s.retained, s.delta_retained));
        }
        t.render(&mut out);
        out.push('\n');
    }

    out
}

/// Thin wrapper: load every report path (version-checked, "-" = stdin), compute
/// the N-way time-series diff, and render it in the requested format.
pub fn run(paths: &[String], format: OutputFormat) -> io::Result<String> {
    let mut reports: Vec<Report> = Vec::with_capacity(paths.len());
    for p in paths {
        reports.push(load_report(p)?);
    }
    let result = diff_series(&reports);
    Ok(match format {
        OutputFormat::Json => serde_json::to_string_pretty(&result).map_err(io::Error::other)?,
        OutputFormat::Html => crate::html::render_diff_html(&result),
        OutputFormat::Md | OutputFormat::MdGraphs => render_md(&result),
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
            max_instance_shallow: 0,
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
            root_path: None,
            dominator_tree: None,
            merged_paths: None,
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
                unreachable_retained: 0,
                unreachable_composition: Default::default(),
                unreachable_garbage_roots: vec![],
                unreachable_histogram: vec![],
                histogram,
                histogram_truncated_to: None,
                system_properties: vec![],
                jvm_version: None,
                loader_rollup: vec![],
                duplicate_classes: vec![],
                record_census: Default::default(),
                duplicate_strings: None,
                heap_fragmentation_ratio: 0.0,
                top_class_concentration_bp: 0,
                gc_roots_retained_by_type: vec![],
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
                size_distribution: Default::default(),
            },
            threads: crate::report::ThreadOverview { threads: vec![] },
            top_components: crate::report::TopComponents::default(),
            alloc_sites: None,
            arrays_by_size: Default::default(),
            dominator_analysis: Default::default(),
            collections: Default::default(),
            references: Default::default(),
            collection_attribution: None,
            fields_by_size: None,
            biggest_collections: None,
            collection_contents: None,
            leak_indicators: Default::default(),
        }
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
        let d = diff_series(&[a, b]);
        let md = render_md(&d);
        assert!(md.contains("## Cross-Dump Growth"));
        // The legend maps r1/r2 to their labels.
        assert!(md.contains("### Reports"));
        assert!(md.contains("`r1` ="));
        assert!(md.contains("`r2` ="));
        assert!(md.contains("**Verdict:**"));
        assert!(md.contains("### Headline Totals"));
        assert!(md.contains("### Growth Leaders (by Δ retained)"));
        assert!(md.contains("### New Classes"));
        assert!(md.contains("### Removed Classes"));
        assert!(md.contains("### New / Grown Leak Suspects"));
        assert!(md.contains("### Shrunk Leak Suspects"));
        assert!(md.contains("### Disappeared Leak Suspects"));
        // Per-report + Δ columns are present.
        assert!(md.contains("Δ(r1→rN)"));
        // The grown class row is present.
        assert!(md.contains("`Foo`"));
        // The new suspect row is present.
        assert!(md.contains("`com.example.Leaky`"));
    }

    #[test]
    fn render_md_n3_has_three_value_columns() {
        let r1 = named_report("d1", 0, 0, vec![hist("Foo", 1, 10, 100)], vec![]);
        let r2 = named_report("d2", 0, 0, vec![hist("Foo", 2, 20, 200)], vec![]);
        let r3 = named_report("d3", 0, 0, vec![hist("Foo", 3, 30, 300)], vec![]);
        let d = diff_series(&[r1, r2, r3]);
        let md = render_md(&d);
        // Three report labels in the legend.
        assert!(md.contains("`r1` = d1"));
        assert!(md.contains("`r2` = d2"));
        assert!(md.contains("`r3` = d3"));
        // Growth-leaders header has r1|r2|r3 value columns + a Δ column.
        // The Table pads cells, so match on a padding-agnostic header line.
        let header = md
            .lines()
            .find(|l| l.contains("Class") && l.contains("Δ(r1→rN)"))
            .expect("a header row with the class + Δ columns");
        let r1 = header.find(" r1 ").expect("r1 column");
        let r2 = header.find(" r2 ").expect("r2 column");
        let r3 = header.find(" r3 ").expect("r3 column");
        let delta = header.find("Δ(r1→rN)").expect("Δ column");
        assert!(r1 < r2 && r2 < r3 && r3 < delta, "header order: {header}");
    }

    #[test]
    fn verdict_grew_names_driver() {
        // r1: shallow 1000; r2: shallow 2000 (grew 100%). Driver `Big`.
        let a = base_report(0, 1_000, vec![hist("Big", 1, 10, 1_000)], vec![]);
        let b = base_report(
            0,
            2_000,
            vec![hist("Big", 5, 50, 2_000)],
            vec![suspect("NewLeak", 3, 500)],
        );
        let d = diff_series(&[a, b]);
        let v = verdict(&d);
        // total shallow +1000 on a 1000-byte baseline = 100%.
        assert!(v.starts_with("Heap grew 100.0%"), "got: {v}");
        assert!(v.contains("largest driver `Big`"), "got: {v}");
        assert!(v.contains("1 new suspect."), "got: {v}");
    }

    #[test]
    fn verdict_shrank() {
        // r1: shallow 2000; r2: shallow 500 (shrank 75%).
        let a = base_report(0, 2_000, vec![hist("Big", 5, 50, 2_000)], vec![]);
        let b = base_report(0, 500, vec![hist("Big", 1, 10, 500)], vec![]);
        let d = diff_series(&[a, b]);
        let v = verdict(&d);
        // total shallow -1500 on a 2000-byte baseline = 75% shrink.
        assert!(v.starts_with("Heap shrank 75.0%"), "got: {v}");
        assert!(v.contains("no net growth"), "got: {v}");
    }

    // Give a report a distinct source_name so we can assert the derived labels.
    fn named_report(
        name: &str,
        total_objects: u64,
        total_shallow: u64,
        histogram: Vec<HistRow>,
        suspects: Vec<Suspect>,
    ) -> Report {
        let mut r = base_report(total_objects, total_shallow, histogram, suspects);
        r.overview.source_name = name.to_string();
        r
    }

    #[test]
    fn series_n2_matches_pairwise_diff() {
        // Same inputs as class_delta_join_grew_shrank_new_removed, but through
        // the N-way engine with N==2 — the first→last numbers must match.
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
        let s = diff_series(&[a, b]);

        // Growth leaders: NewClass (+400) then Grew (+200), same as pairwise.
        assert_eq!(s.growth_leaders.len(), 2);
        assert_eq!(s.growth_leaders[0].pretty_class, "NewClass");
        assert_eq!(s.growth_leaders[0].delta_retained, 400);
        assert_eq!(s.growth_leaders[0].retained, vec![0, 400]);
        assert_eq!(s.growth_leaders[1].pretty_class, "Grew");
        assert_eq!(s.growth_leaders[1].delta_retained, 200);
        assert_eq!(s.growth_leaders[1].delta_instances, 2);
        assert_eq!(s.growth_leaders[1].retained, vec![100, 300]);
        assert!(
            !s.growth_leaders
                .iter()
                .any(|c| c.pretty_class == "Shrank" || c.pretty_class == "Removed")
        );

        // New class: NewClass, retained [0, 400].
        assert_eq!(s.new_classes.len(), 1);
        assert_eq!(s.new_classes[0].pretty_class, "NewClass");
        assert_eq!(s.new_classes[0].retained, vec![0, 400]);

        // Removed class: Removed, retained [200, 0].
        assert_eq!(s.removed_classes.len(), 1);
        assert_eq!(s.removed_classes[0].pretty_class, "Removed");
        assert_eq!(s.removed_classes[0].retained, vec![200, 0]);

        // Two reports => two labels, in input order.
        assert_eq!(s.labels.len(), 2);
    }

    #[test]
    fn series_n2_suspects_match_pairwise() {
        // Mirror suspect_delta_new_and_grown_sorted through the N-way engine.
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
                suspect("GrownSuspect", 20, 3_000),
                suspect("Stable", 5, 500),
                suspect("BrandNew", 8, 5_000),
            ],
        );
        let s = diff_series(&[a, b]);
        assert_eq!(s.grown_suspects.len(), 2);
        assert_eq!(s.grown_suspects[0].pretty_class, "BrandNew");
        assert!(s.grown_suspects[0].is_new);
        assert_eq!(s.grown_suspects[0].retained, vec![0, 5_000]);
        assert_eq!(s.grown_suspects[1].pretty_class, "GrownSuspect");
        assert!(!s.grown_suspects[1].is_new);
        assert_eq!(s.grown_suspects[1].delta_retained, 2_000);
        assert!(!s.grown_suspects.iter().any(|s| s.pretty_class == "Stable"));
    }

    #[test]
    fn series_n3_time_series() {
        // r1 < r2 < r3 for "Climber"; "OnlyR3" new in r3; "OnlyR1" in r1 only.
        // Suspect "LateLeak" new in r3; suspect "EarlyLeak" gone by r3.
        let r1 = named_report(
            "dump1.hprof",
            10,
            1_000,
            vec![hist("Climber", 1, 10, 100), hist("OnlyR1", 2, 20, 250)],
            vec![suspect("EarlyLeak", 3, 900)],
        );
        let r2 = named_report(
            "dump2.hprof",
            20,
            2_000,
            vec![hist("Climber", 2, 20, 200)],
            vec![suspect("EarlyLeak", 3, 900)],
        );
        let r3 = named_report(
            "dump3.hprof",
            30,
            3_000,
            vec![hist("Climber", 3, 30, 300), hist("OnlyR3", 5, 50, 700)],
            vec![suspect("LateLeak", 4, 1_500)],
        );
        let s = diff_series(&[r1, r2, r3]);

        // Labels derived from source_name, in input order.
        assert_eq!(s.labels, vec!["dump1.hprof", "dump2.hprof", "dump3.hprof"]);
        // Headline series len N, first->last deltas.
        assert_eq!(s.total_objects, vec![10, 20, 30]);
        assert_eq!(s.total_shallow, vec![1_000, 2_000, 3_000]);
        assert_eq!(s.delta_total_objects, 20);
        assert_eq!(s.delta_total_shallow, 2_000);

        // Climber grows monotonically and leads growth (Δ = 300 - 100 = 200).
        let climber = s
            .growth_leaders
            .iter()
            .find(|c| c.pretty_class == "Climber")
            .expect("Climber is a growth leader");
        assert_eq!(climber.retained, vec![100, 200, 300]);
        assert_eq!(climber.instances, vec![1, 2, 3]);
        assert_eq!(climber.delta_retained, 200);
        assert_eq!(climber.delta_instances, 2);

        // OnlyR3 is new (absent in first, present in last), retained [0,0,700].
        assert_eq!(s.new_classes.len(), 1);
        assert_eq!(s.new_classes[0].pretty_class, "OnlyR3");
        assert_eq!(s.new_classes[0].retained, vec![0, 0, 700]);

        // OnlyR1 is removed (present in first, absent in last), retained [250,0,0].
        assert_eq!(s.removed_classes.len(), 1);
        assert_eq!(s.removed_classes[0].pretty_class, "OnlyR1");
        assert_eq!(s.removed_classes[0].retained, vec![250, 0, 0]);

        // LateLeak is a new-in-last suspect.
        let late = s
            .grown_suspects
            .iter()
            .find(|s| s.pretty_class == "LateLeak")
            .expect("LateLeak is a grown/new suspect");
        assert!(late.is_new);
        assert!(!late.is_gone);
        assert_eq!(late.retained, vec![0, 0, 1_500]);

        // EarlyLeak is gone by r3 (present in first, absent in last).
        assert_eq!(s.gone_suspects.len(), 1);
        assert_eq!(s.gone_suspects[0].pretty_class, "EarlyLeak");
        assert!(s.gone_suspects[0].is_gone);
        assert_eq!(s.gone_suspects[0].retained, vec![900, 900, 0]);
    }
}

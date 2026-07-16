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
/// it is entirely new in B, or that it disappeared (present in A, absent in B).
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct SuspectDelta {
    pub pretty_class: String,
    pub a_retained: u64,
    pub b_retained: u64,
    pub delta_retained: i64,
    pub is_new: bool,
    /// Present in A, absent from B — the suspect no longer retains anything.
    #[serde(default)]
    pub is_gone: bool,
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
    /// Classes present in A but absent from B (dropped out of the heap).
    pub removed_classes: Vec<ClassDelta>,
    pub grown_suspects: Vec<SuspectDelta>,
    /// Suspects present in both dumps whose retained heap fell.
    pub shrunk_suspects: Vec<SuspectDelta>,
    /// Suspects present in A but absent from B (no longer a suspect).
    pub gone_suspects: Vec<SuspectDelta>,
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
    let mut removed_classes: Vec<ClassDelta> = Vec::new();
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
        // A class is "new" iff present in B and absent from A; "removed" iff the
        // reverse. Both are mutually exclusive with each other.
        if p.b_present && !p.a_present {
            new_classes.push(cd.clone());
        } else if p.a_present && !p.b_present {
            removed_classes.push(cd.clone());
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

    // Removed classes: sorted by A.retained descending (biggest thing that
    // dropped out first), then name ascending.
    removed_classes.sort_by(|x, y| {
        y.a_retained
            .cmp(&x.a_retained)
            .then_with(|| x.pretty_class.cmp(&y.pretty_class))
    });
    removed_classes.truncate(TOP_N);

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
    let mut shrunk_suspects: Vec<SuspectDelta> = Vec::new();
    for (name, &b_ret) in &b_best {
        let a_ret_opt = a_best.get(name).copied();
        let is_new = a_ret_opt.is_none();
        let a_ret = a_ret_opt.unwrap_or(0);
        let delta_retained = b_ret as i64 - a_ret as i64;
        // New in B or grown → growth list; present in both but shrank → shrink list.
        if is_new || delta_retained > 0 {
            grown_suspects.push(SuspectDelta {
                pretty_class: (*name).to_string(),
                a_retained: a_ret,
                b_retained: b_ret,
                delta_retained,
                is_new,
                is_gone: false,
            });
        } else if !is_new && delta_retained < 0 {
            shrunk_suspects.push(SuspectDelta {
                pretty_class: (*name).to_string(),
                a_retained: a_ret,
                b_retained: b_ret,
                delta_retained,
                is_new: false,
                is_gone: false,
            });
        }
    }
    grown_suspects.sort_by(|x, y| {
        y.b_retained
            .cmp(&x.b_retained)
            .then_with(|| x.pretty_class.cmp(&y.pretty_class))
    });
    // Shrunk suspects: most-negative delta first (biggest reduction), then name.
    shrunk_suspects.sort_by(|x, y| {
        x.delta_retained
            .cmp(&y.delta_retained)
            .then_with(|| x.pretty_class.cmp(&y.pretty_class))
    });

    // Gone suspects: present in A, absent from B — no longer retaining anything.
    let mut gone_suspects: Vec<SuspectDelta> = Vec::new();
    for (name, &a_ret) in &a_best {
        if !b_best.contains_key(name) {
            gone_suspects.push(SuspectDelta {
                pretty_class: (*name).to_string(),
                a_retained: a_ret,
                b_retained: 0,
                delta_retained: -(a_ret as i64),
                is_new: false,
                is_gone: true,
            });
        }
    }
    gone_suspects.sort_by(|x, y| {
        y.a_retained
            .cmp(&x.a_retained)
            .then_with(|| x.pretty_class.cmp(&y.pretty_class))
    });

    DiffReportsResult {
        delta_total_objects: b.overview.total_objects as i64 - a.overview.total_objects as i64,
        delta_total_shallow: b.overview.total_shallow as i64 - a.overview.total_shallow as i64,
        net_delta_retained,
        growth_leaders,
        new_classes,
        removed_classes,
        grown_suspects,
        shrunk_suspects,
        gone_suspects,
    }
}

// ── N-way time-series diff ──────────────────────────────────────────────────

/// One joined class row across N reports: its instances/retained at each report
/// (index 0 = first, N−1 = last), plus the first→last deltas.
// Engine-only for now: renderers/CLI wire these in later. Until then the
// public items are only exercised by unit tests, so silence dead_code.
#[allow(dead_code)]
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
#[allow(dead_code)]
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
#[allow(dead_code)]
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
#[allow(dead_code)]
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
/// `a_total_shallow` is the baseline's reachable heap. The grew/shrank magnitude
/// uses the change in *total shallow heap* (the real, bounded heap size) as its
/// basis — net retained sums per-class retention, which overlaps and can exceed
/// the heap, so it is unsuitable as a percentage denominator. The largest
/// *retained* driver is still named, as it best explains the change. Pure and
/// deterministic — integers and a single rounded percentage only.
fn verdict(d: &DiffReportsResult, a_total_shallow: u64) -> String {
    let pct = if a_total_shallow > 0 {
        d.delta_total_shallow as f64 / a_total_shallow as f64 * 100.0
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

/// Render the diff as human-readable Markdown.
pub fn render_md(d: &DiffReportsResult, a_total_shallow: u64) -> String {
    let mut out = String::new();
    out.push_str("## Cross-Dump Growth\n\n");
    out.push_str(
        "_How the reachable heap grew from the baseline (A) to the current (B) dump._\n\n",
    );

    out.push_str(&format!("**Verdict:** {}\n\n", verdict(d, a_total_shallow)));

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

    out.push_str("### Removed Classes\n\n");
    if d.removed_classes.is_empty() {
        out.push_str("No classes dropped out of the current dump.\n\n");
    } else {
        let mut t = Table::new(
            &["Class", "Instances (A)", "Retained (A)"],
            &[Align::Left, Align::Right, Align::Right],
        );
        for c in &d.removed_classes {
            // A-side instance count = |delta| here (b_inst is 0 for removed).
            t.row([
                format!("`{}`", c.pretty_class),
                fmt_delta_count(-c.delta_instances),
                report::format_bytes(c.a_retained),
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

    out.push_str("### Shrunk Leak Suspects\n\n");
    if d.shrunk_suspects.is_empty() {
        out.push_str("No leak suspect shrank in the current dump.\n\n");
    } else {
        let mut t = Table::new(
            &["Suspect", "Retained (A)", "Retained (B)", "Δ Retained"],
            &[Align::Left, Align::Right, Align::Right, Align::Right],
        );
        for s in &d.shrunk_suspects {
            t.row([
                format!("`{}`", s.pretty_class),
                report::format_bytes(s.a_retained),
                report::format_bytes(s.b_retained),
                fmt_delta_bytes(s.delta_retained),
            ]);
        }
        t.render(&mut out);
        out.push('\n');
    }

    out.push_str("### Disappeared Leak Suspects\n\n");
    if d.gone_suspects.is_empty() {
        out.push_str("No leak suspect disappeared in the current dump.\n\n");
    } else {
        let mut t = Table::new(&["Suspect", "Retained (A)"], &[Align::Left, Align::Right]);
        for s in &d.gone_suspects {
            t.row([
                format!("`{}`", s.pretty_class),
                report::format_bytes(s.a_retained),
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
        OutputFormat::Md | OutputFormat::MdGraphs | OutputFormat::Html => {
            render_md(&result, a.overview.total_shallow)
        }
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
            leak_indicators: Default::default(),
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
        let md = render_md(&d, a.overview.total_shallow);
        assert!(md.contains("## Cross-Dump Growth"));
        assert!(md.contains("**Verdict:**"));
        assert!(md.contains("### Headline Totals"));
        assert!(md.contains("### Growth Leaders (by Δ retained)"));
        assert!(md.contains("### New Classes"));
        assert!(md.contains("### Removed Classes"));
        assert!(md.contains("### New / Grown Leak Suspects"));
        assert!(md.contains("### Shrunk Leak Suspects"));
        assert!(md.contains("### Disappeared Leak Suspects"));
        // The grown class row is present.
        assert!(md.contains("`Foo`"));
        // The new suspect row is present.
        assert!(md.contains("`com.example.Leaky`"));
    }

    #[test]
    fn removed_class_and_shrunk_gone_suspects() {
        let a = base_report(
            0,
            10_000,
            vec![hist("Keep", 1, 10, 100), hist("Dropped", 4, 40, 400)],
            vec![suspect("Shrinks", 10, 3_000), suspect("Vanishes", 5, 2_000)],
        );
        let b = base_report(
            0,
            0,
            vec![hist("Keep", 1, 10, 100)],
            vec![suspect("Shrinks", 4, 1_000)], // 3000 -> 1000
        );
        let d = diff(&a, &b);

        // "Dropped" is present in A only.
        assert_eq!(d.removed_classes.len(), 1);
        assert_eq!(d.removed_classes[0].pretty_class, "Dropped");
        assert_eq!(d.removed_classes[0].a_retained, 400);

        // "Shrinks" fell 3000 -> 1000.
        assert_eq!(d.shrunk_suspects.len(), 1);
        assert_eq!(d.shrunk_suspects[0].pretty_class, "Shrinks");
        assert_eq!(d.shrunk_suspects[0].delta_retained, -2_000);
        assert!(!d.shrunk_suspects[0].is_new);
        assert!(!d.shrunk_suspects[0].is_gone);

        // "Vanishes" is present in A only.
        assert_eq!(d.gone_suspects.len(), 1);
        assert_eq!(d.gone_suspects[0].pretty_class, "Vanishes");
        assert_eq!(d.gone_suspects[0].a_retained, 2_000);
        assert_eq!(d.gone_suspects[0].b_retained, 0);
        assert!(d.gone_suspects[0].is_gone);
        // A shrunk suspect must not also appear as gone or grown.
        assert!(!d.grown_suspects.iter().any(|s| s.pretty_class == "Shrinks"));
    }

    #[test]
    fn verdict_grew_names_driver() {
        // A: shallow 1000; B: shallow 2000 (grew 100%). Retained driver `Big`.
        let a = base_report(0, 1_000, vec![hist("Big", 1, 10, 1_000)], vec![]);
        let b = base_report(
            0,
            2_000,
            vec![hist("Big", 5, 50, 2_000)],
            vec![suspect("NewLeak", 3, 500)],
        );
        let d = diff(&a, &b);
        let v = verdict(&d, a.overview.total_shallow);
        // total shallow +1000 on a 1000-byte baseline = 100%.
        assert!(v.starts_with("Heap grew 100.0%"), "got: {v}");
        assert!(v.contains("largest driver `Big`"), "got: {v}");
        assert!(v.contains("1 new suspect."), "got: {v}");
    }

    #[test]
    fn verdict_shrank() {
        // A: shallow 2000; B: shallow 500 (shrank 75%).
        let a = base_report(0, 2_000, vec![hist("Big", 5, 50, 2_000)], vec![]);
        let b = base_report(0, 500, vec![hist("Big", 1, 10, 500)], vec![]);
        let d = diff(&a, &b);
        let v = verdict(&d, a.overview.total_shallow);
        // total shallow -1500 on a 2000-byte baseline = 75% shrink.
        assert!(v.starts_with("Heap shrank 75.0%"), "got: {v}");
        assert!(v.contains("no net growth"), "got: {v}");
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

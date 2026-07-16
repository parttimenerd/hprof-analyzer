//! Plain-Markdown renderers plus the record-census / duplicate-string
//! sections shared with the graphs renderer.

use super::*;

/// Render the "HPROF Record Census" subsection: a key/value table of raw
/// record-type counts plus a per-GC-root-tag breakdown. Identical output in the
/// plain-md and graphs-md renderers (plain counts, no bars). Additive.
pub(crate) fn render_record_census(out: &mut String, c: &crate::pass2::RecordCensus) {
    use crate::md::{Align, Table};
    out.push_str("### HPROF Record Census\n\n");
    out.push_str(
        "_Raw HPROF record-type composition of the dump (pass-1 counts); \
         additive, not parity-compared._\n\n",
    );
    let mut t = Table::new(&["Record Type", "Count"], &[Align::Left, Align::Right]);
    t.row(["UTF8 strings".into(), fmt_count(c.utf8_records)]);
    t.row(["Load class".into(), fmt_count(c.load_class_records)]);
    t.row(["Unload class".into(), fmt_count(c.unload_class_records)]);
    t.row(["Stack frames".into(), fmt_count(c.stack_frame_records)]);
    t.row(["Stack traces".into(), fmt_count(c.stack_trace_records)]);
    t.row(["Heap dump segments".into(), fmt_count(c.heap_dump_segments)]);
    t.row(["Instance dumps".into(), fmt_count(c.instance_dumps)]);
    t.row(["Object-array dumps".into(), fmt_count(c.obj_array_dumps)]);
    t.row([
        "Primitive-array dumps".into(),
        fmt_count(c.prim_array_dumps),
    ]);
    t.row(["Class dumps".into(), fmt_count(c.class_dumps)]);
    t.render(out);
    out.push('\n');

    if !c.gc_root_tag_counts.is_empty() {
        out.push_str("#### GC Root Records by Tag\n\n");
        let mut t = Table::new(&["Root Tag", "Count"], &[Align::Left, Align::Right]);
        for &(tag, count) in &c.gc_root_tag_counts {
            t.row([gc_root_type_label(tag).to_string(), fmt_count(count)]);
        }
        t.render(out);
        out.push('\n');
    }
}

/// Render the opt-in approximate duplicate-`java.lang.String` block. The
/// section header is always emitted (so it is discoverable); when the analysis
/// was not requested (`None`) it renders a one-line "not run" note instead of
/// stats.
pub(crate) fn render_duplicate_strings(
    out: &mut String,
    d: &Option<crate::pass2::DupStrings>,
    graphs: bool,
) {
    use crate::md::{bar, sparkline, Align, Table};
    out.push_str("### Duplicate Strings (approximate)\n\n");
    let d = match d {
        None => {
            out.push_str("_Duplicate-string analysis not run (pass `--dup-strings`)._\n\n");
            return;
        }
        Some(d) => d,
    };
    out.push_str(
        "_Opt-in (`--dup-strings`): each `java.lang.String` value hashed to \
         64 bits; collisions accepted as approximation._\n\n",
    );
    out.push_str(&format!(
        "- Total String instances: {}\n",
        fmt_count(d.total_string_instances)
    ));
    out.push_str(&format!(
        "- Distinct values: {}\n",
        fmt_count(d.distinct_values)
    ));
    out.push_str(&format!(
        "- Duplicated values: {}\n",
        fmt_count(d.duplicated_values)
    ));
    out.push_str(&format!(
        "- Approx wasted bytes: {}\n\n",
        format_bytes(d.approx_wasted_bytes)
    ));

    // ── Most-duplicated string values (exact, truncated text) ────────────────
    if !d.top_duplicated.is_empty() {
        out.push_str("#### Most-Duplicated Values\n\n");
        let mut t = Table::new(
            &["#", "Count", "Wasted", "Value"],
            &[Align::Right, Align::Right, Align::Right, Align::Left],
        );
        for (i, s) in d.top_duplicated.iter().enumerate() {
            t.row([
                format!("{}", i + 1),
                fmt_count(s.count),
                format_bytes(s.wasted_bytes),
                format!("`{}`", escape_string_cell(&s.text)),
            ]);
        }
        t.render(out);
        out.push('\n');
    }

    // ── Longest distinct string values (exact, truncated text) ───────────────
    if !d.top_by_length.is_empty() {
        out.push_str("#### Longest Values\n\n");
        let mut t = Table::new(
            &["#", "Length", "Count", "Value"],
            &[Align::Right, Align::Right, Align::Right, Align::Left],
        );
        for (i, s) in d.top_by_length.iter().enumerate() {
            t.row([
                format!("{}", i + 1),
                fmt_count(s.len as u64),
                fmt_count(s.count),
                format!("`{}`", escape_string_cell(&s.text)),
            ]);
        }
        t.render(out);
        out.push('\n');
    }

    // ── String-length histogram ──────────────────────────────────────────────
    if !d.length_histogram.is_empty() {
        out.push_str("#### String Length Distribution\n\n");
        out.push_str(&format!(
            "_Distinct-value lengths (bytes): min {}, median {}, max {}; total {}._\n\n",
            fmt_count(d.length_stats.min as u64),
            fmt_count(d.length_stats.median as u64),
            fmt_count(d.length_stats.max as u64),
            format_bytes(d.length_stats.total),
        ));
        let counts: Vec<u64> = d.length_histogram.iter().map(|b| b.count).collect();
        if graphs {
            out.push_str(&format!("`{}`\n\n", sparkline(&counts)));
            let bmax = counts.iter().copied().max().unwrap_or(0);
            let mut t = Table::new(
                &["Length ≤", "Values", ""],
                &[Align::Right, Align::Right, Align::Left],
            );
            for b in &d.length_histogram {
                t.row([
                    fmt_count(b.upper_len as u64),
                    fmt_count(b.count),
                    bar(b.count, bmax, GRAPH_BAR_WIDTH),
                ]);
            }
            t.render(out);
        } else {
            let mut t = Table::new(&["Length ≤", "Values"], &[Align::Right, Align::Right]);
            for b in &d.length_histogram {
                t.row([fmt_count(b.upper_len as u64), fmt_count(b.count)]);
            }
            t.render(out);
        }
        out.push('\n');
    }

    // ── Classes holding the most Strings ─────────────────────────────────────
    if !d.top_string_holders.is_empty() {
        out.push_str("#### Classes Holding the Most Strings\n\n");
        out.push_str(
            "_Number of `java.lang.String` instances referenced by each class's instances._\n\n",
        );
        let mut t = Table::new(&["Class", "String refs"], &[Align::Left, Align::Right]);
        for h in &d.top_string_holders {
            t.row([format!("`{}`", h.class_name), fmt_count(h.string_refs)]);
        }
        t.render(out);
        out.push('\n');
    }

    // ── Char[] backing-array waste ───────────────────────────────────────────
    if let Some(w) = &d.char_array_waste {
        out.push_str("#### Char[] Waste\n\n");
        out.push_str(&format!(
            "_{} arrays examined, {} wasteful, {} total wasted._\n\n",
            fmt_count(w.arrays_examined),
            fmt_count(w.wasteful_arrays),
            format_bytes(w.total_wasted_bytes),
        ));
        if !w.top.is_empty() {
            let mut t = Table::new(
                &["Array #", "Length", "Used", "Wasted"],
                &[Align::Right, Align::Right, Align::Right, Align::Right],
            );
            for r in &w.top {
                t.row([
                    fmt_count(r.array_obj_1based as u64),
                    fmt_count(r.length),
                    format_bytes(r.used),
                    format_bytes(r.wasted_bytes),
                ]);
            }
            t.render(out);
            out.push('\n');
        }
    }
}

/// If the single largest suspect retains at least this share of the reachable
/// heap, the OOM-triage lead-in calls the heap "dominated" by one retainer.
const CONCENTRATION_PCT: f64 = 50.0;

// ── Rendering ────────────────────────────────────────────────────────────────

/// Render the "Leak Indicators" section (plain Markdown): scalar counters for
/// anonymous classes, ThreadLocal null-key entries, and DirectByteBuffer total
/// capacity. Only emitted when at least one indicator is non-zero.
pub(crate) fn render_leak_indicators(li: &crate::report::LeakIndicators, out: &mut String) {
    if li.anonymous_class_count == 0
        && li.thread_local_null_key_count == 0
        && li.direct_byte_buffer_capacity_sum == 0
    {
        return;
    }
    use crate::md::{Align, Table};
    out.push_str("## Leak Indicators\n\n");
    out.push_str(
        "_Scalar signals for common Java leak patterns. Non-zero values here \
         are worth investigating._\n\n",
    );
    let mut t = Table::new(&["Indicator", "Value"], &[Align::Left, Align::Right]);
    if li.anonymous_class_count > 0 {
        t.row([
            "Anonymous/generated classes".into(),
            fmt_count(li.anonymous_class_count),
        ]);
    }
    if li.thread_local_null_key_count > 0 {
        t.row([
            "ThreadLocal null-key entries (cleared referent)".into(),
            fmt_count(li.thread_local_null_key_count),
        ]);
    }
    if li.direct_byte_buffer_capacity_sum > 0 {
        t.row([
            "DirectByteBuffer total capacity".into(),
            format_bytes(li.direct_byte_buffer_capacity_sum),
        ]);
    }
    t.render(out);
    out.push('\n');
}

/// Render a `Report` into Markdown. Byte-identical to the previous
/// `system_overview` + `leak_suspects` + `top_consumers` concatenation.
pub fn render_markdown(r: &Report) -> String {
    let mut out = String::new();
    render_title(&r.overview, &r.generated, &mut out);
    render_toc(r, &mut out);
    render_executive_summary(r, &mut out);
    render_oom_triage(r, &mut out);
    render_system_overview(&r.overview, &mut out);
    render_leak_suspects(&r.leaks, &mut out);
    render_top_consumers(&r.top, r.leaks.total_shallow, &mut out);
    render_dominator_analysis(&r.dominator_analysis, false, &mut out);
    render_threads(&r.threads, false, &mut out);
    render_top_components(&r.top_components, false, &mut out);
    render_arrays_by_size(&r.arrays_by_size, false, &mut out);
    render_collections(&r.collections, false, &mut out);
    render_collection_attribution(&r.collection_attribution, false, &mut out);
    render_references(&r.references, false, &mut out);
    render_unreachable_histogram(&r.overview, false, &mut out);
    // Allocation sites (always present; `None` only for legacy reports).
    if let Some(a) = &r.alloc_sites {
        render_alloc_sites(a, false, &mut out);
    }
    render_retention_concentration(&r.overview, &mut out);
    render_dominator_depth(&r.overview, &mut out);
    render_leak_indicators(&r.leak_indicators, &mut out);
    render_glossary(&mut out);
    out
}

/// Linked in-document table of contents (top-level sections only). Anchors use
/// GitHub's slug convention (lowercase, spaces → hyphens) matching the `##`
/// headings emitted by the section renderers. Kept in lock-step with
/// `render_toc_graphs` so both formats list the same sections.
fn render_toc(r: &Report, out: &mut String) {
    out.push_str("## Contents\n\n");
    out.push_str("- [Summary](#summary)\n");
    out.push_str("- [OOM Triage](#oom-triage)\n");
    out.push_str("- [System Overview](#system-overview)\n");
    out.push_str("- [Leak Suspects](#leak-suspects)\n");
    out.push_str("- [Top Consumers](#top-consumers)\n");
    out.push_str("- [Dominator Analysis](#dominator-analysis)\n");
    out.push_str("- [Threads](#threads)\n");
    if !r.top_components.components.is_empty() {
        out.push_str("- [Top Components](#top-components)\n");
    }
    out.push_str("- [Arrays by Size](#arrays-by-size)\n");
    out.push_str("- [Collections](#collections)\n");
    if r.collection_attribution.is_some() {
        out.push_str(
            "- [Container Attribution (Class#field)](#container-attribution-classfield)\n",
        );
    }
    out.push_str("- [References](#references)\n");
    out.push_str("- [Unreachable Objects](#unreachable-objects)\n");
    if r.alloc_sites.is_some() {
        out.push_str("- [Allocation Sites](#allocation-sites)\n");
    }
    if retention_concentration_present(&r.overview) {
        out.push_str("- [Retention Concentration](#retention-concentration)\n");
    }
    if depth_stats(&r.overview.dominator_depth_histogram).is_some() {
        out.push_str("- [Dominator-Depth Distribution](#dominator-depth-distribution)\n");
    }
    out.push_str("- [Glossary](#glossary)\n");
    out.push('\n');
    out.push_str("----\n\n");
}

/// Emit the document title + generation timestamp + horizontal rule.
/// between the title and the first section.
pub(crate) fn render_title(o: &SystemOverview, generated: &str, out: &mut String) {
    out.push_str(&format!("# Heap Dump Analysis: `{}`\n\n", o.source_name));
    out.push_str(&format!(
        "*Generated by hprof-analyzer views — {}*\n\n",
        generated
    ));
    out.push_str("----\n\n");
}

/// Executive summary: a scannable digest at the very top of the report, before
/// the detailed sections. Two compact mini-tables (a handful of rows each)
/// re-project data already in the model — the headline scalars from System
/// Overview and the top few retainers by retained heap — so a reader gets an
/// at-a-glance answer to "what caused the OOM / where is the heap concentrated?"
/// without scrolling. The full detail tables follow unchanged below. Pure
/// function of `Report` (no new model fields, no graph access).
pub(crate) fn render_executive_summary(r: &Report, out: &mut String) {
    use crate::md::{Align, Table};
    /// Rows shown in the top-suspects digest; the full lists follow below.
    const SUMMARY_SUSPECTS: usize = 5;

    out.push_str("## Summary\n\n");
    out.push_str("_At-a-glance digest; see the sections below for full detail._\n\n");

    // Key stats: the headline scalars the System Overview already exposes.
    let o = &r.overview;
    let mut stats = Table::new(&["Metric", "Value"], &[Align::Left, Align::Right]);
    stats.row([
        "Total heap (reachable)".into(),
        format_bytes(o.total_shallow),
    ]);
    stats.row(["Objects".into(), fmt_count(o.total_objects)]);
    stats.row(["Classes".into(), fmt_count(o.classes_loaded)]);
    stats.row(["Class loaders".into(), fmt_count(o.classloaders_loaded)]);
    stats.row(["Threads".into(), fmt_count(r.threads.threads.len() as u64)]);
    stats.row(["GC roots".into(), fmt_count(o.gc_roots)]);
    stats.render(out);
    out.push('\n');

    // Top suspects / biggest retained: the single most important OOM signal,
    // shown up front. Prefer the leak-suspects list; fall back to the biggest
    // top-level objects when no suspect exceeds the threshold. Percentage basis
    // matches the detail tables: retained / total reachable shallow heap.
    let total = r.leaks.total_shallow;
    let pct_of = |retained: u64| -> f64 {
        if total > 0 {
            retained as f64 / total as f64 * 100.0
        } else {
            0.0
        }
    };

    if !r.leaks.suspects.is_empty() {
        out.push_str("**Top suspects by retained heap**\n\n");
        let mut t = Table::new(
            &["#", "Suspect", "Retained", "% Heap"],
            &[Align::Right, Align::Left, Align::Right, Align::Right],
        );
        for (rank, s) in r.leaks.suspects.iter().take(SUMMARY_SUSPECTS).enumerate() {
            let what = if s.is_single {
                format!("`{}` (single object)", s.pretty_class)
            } else {
                format!(
                    "`{}` ({} instances)",
                    s.pretty_class,
                    fmt_count(s.instance_count)
                )
            };
            t.row([
                (rank + 1).to_string(),
                what,
                format_bytes(s.retained),
                format!("{:.1}%", pct_of(s.retained)),
            ]);
        }
        t.render(out);
    } else if !r.top.biggest_objects.is_empty() {
        out.push_str("**Biggest retained objects**\n\n");
        let mut t = Table::new(
            &["#", "Class", "Retained", "% Heap"],
            &[Align::Right, Align::Left, Align::Right, Align::Right],
        );
        for (rank, ob) in r
            .top
            .biggest_objects
            .iter()
            .take(SUMMARY_SUSPECTS)
            .enumerate()
        {
            t.row([
                (rank + 1).to_string(),
                format!("`{}`", ob.display_class),
                format_bytes(ob.retained),
                format!("{:.1}%", pct_of(ob.retained)),
            ]);
        }
        t.render(out);
    } else {
        out.push_str("_No dominant retainer found._\n");
    }
    out.push('\n');

    // Plain-language verdict: turn the numbers above into one actionable line so
    // a reader who scans nothing else still learns where to look first. Derived
    // entirely from the suspects list already rendered — no new data.
    let likely = match r.leaks.suspects.first() {
        Some(s) if pct_of(s.retained) >= CONCENTRATION_PCT => format!(
            "**Likely problem:** `{}` retains {:.1}% of the reachable heap — investigate this first.",
            s.pretty_class,
            pct_of(s.retained),
        ),
        Some(_) => {
            "**Likely problem:** retention is spread across several roots; no single object dominates."
                .to_string()
        }
        None => {
            "**Likely problem:** no dominant retainer; the heap looks evenly distributed."
                .to_string()
        }
    };
    out.push_str(&likely);
    out.push_str("\n\n");
}

/// OOM-triage lead-in: a short, human-readable summary re-projecting data
/// already in the model (no new model fields). Names the dominant retainer
/// and characterises how concentrated retention is. Pure function of `Report`.
pub(crate) fn render_oom_triage(r: &Report, out: &mut String) {
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
            "- **Headline retainer:** `{}` ({}) retains {} ({:.1}% of reachable heap). See [Leak Suspects](#leak-suspects).\n",
            s.pretty_class,
            kind,
            format_bytes(s.retained),
            pct_of(s.retained),
        ));
    } else if let Some(o) = r.top.biggest_objects.first() {
        out.push_str(&format!(
            "- **Headline retainer:** `{}` retains {} ({:.1}% of reachable heap). See [Top Consumers](#top-consumers).\n",
            o.display_class,
            format_bytes(o.retained),
            pct_of(o.retained),
        ));
    } else {
        out.push_str("- **Headline retainer:** No dominant retainer found.\n");
    }

    // Concentration hint: derived purely from the suspects list. Names the
    // dominating group and links to Leak Suspects so the reader can jump to it.
    match r.leaks.suspects.first() {
        Some(s) if pct_of(s.retained) >= CONCENTRATION_PCT => {
            let kind = if s.is_single {
                "a single object"
            } else {
                "a class group"
            };
            out.push_str(&format!(
                "- **Concentration:** highly concentrated — `{}` ({}) holds {:.1}% of the heap, so freeing it would reclaim most memory. See [Leak Suspects](#leak-suspects).\n",
                s.pretty_class,
                kind,
                pct_of(s.retained),
            ));
        }
        Some(_) => {
            out.push_str(
                "- **Concentration:** diffuse — retention is spread across multiple roots, so there is no single object to free. See [Leak Suspects](#leak-suspects).\n",
            );
        }
        None => {
            out.push_str(
                "- **Concentration:** diffuse — no suspect exceeds the threshold; retention is spread across many roots.\n",
            );
        }
    }

    // Shape (B2): shallow vs. deep retention, from the dominator-depth histogram.
    let hist = &r.overview.dominator_depth_histogram;
    if !hist.is_empty() {
        let total: u64 = hist.iter().map(|b| b.objects).sum();
        let max_depth = hist.iter().map(|b| b.depth).max().unwrap_or(0);
        // p90 depth: smallest depth whose cumulative count reaches 90%.
        let mut cum = 0u64;
        let mut p90 = max_depth;
        for b in hist {
            cum += b.objects;
            if cum * 10 >= total * 9 {
                p90 = b.depth;
                break;
            }
        }
        let shape = if p90 <= 3 {
            "shallow (most objects are held within a few hops of a GC root)"
        } else {
            "deep (retention flows through long dominator chains — often nested collections or linked structures)"
        };
        out.push_str(&format!(
            "- **Shape:** {shape} — 90% of objects within depth {p90}, max depth {max_depth}. See [Dominator-Depth Distribution](#dominator-depth-distribution).\n"
        ));
    }

    // One leak or many (B3): from the retention-concentration summary. Names the
    // single biggest object (from Top Consumers) and links to that section.
    let rc = &r.overview.retention_concentration;
    if rc.top1_bp > 0 || rc.num_objects_ge_1pct > 0 {
        let top1_pct = rc.top1_bp as f64 / 100.0;
        let top10_pct = rc.top10_bp as f64 / 100.0;
        let biggest = r
            .top
            .biggest_objects
            .first()
            .map(|o| format!("`{}`", o.display_class));
        match biggest {
            Some(name) => out.push_str(&format!(
                "- **One leak or many:** the single biggest object, {}, retains {:.1}% and the top 10 retain {:.1}% of the heap; {} object(s) each hold >=1%. See [Top Consumers](#top-consumers).\n",
                name, top1_pct, top10_pct, rc.num_objects_ge_1pct,
            )),
            None => out.push_str(&format!(
                "- **One leak or many:** the single biggest object retains {:.1}% and the top 10 retain {:.1}% of the heap; {} object(s) each hold >=1%. See [Top Consumers](#top-consumers).\n",
                top1_pct, top10_pct, rc.num_objects_ge_1pct,
            )),
        }
    }
    out.push('\n');
}

/// Whether the Retention Concentration section has any data to render. Shared by
/// both renderers (and the graphs ToC) so presence stays in lock-step.
pub(crate) fn retention_concentration_present(o: &SystemOverview) -> bool {
    let rc = &o.retention_concentration;
    rc.top1_bp > 0 || rc.top10_bp > 0 || rc.top100_bp > 0 || rc.num_objects_ge_1pct > 0
}

/// Retention Concentration (B3): how much of the heap the few biggest top-level
/// dominators hold. Rendered as a standalone section near the end of the report.
/// Basis points → percent (100 bp = 1%).
pub(crate) fn render_retention_concentration(o: &SystemOverview, out: &mut String) {
    use crate::md::{Align, Table};
    let rc = &o.retention_concentration;
    if !retention_concentration_present(o) {
        return;
    }
    out.push_str("## Retention Concentration\n\n");
    out.push_str(
        "_Share of the reachable heap retained by the few largest top-level dominators \
         (a dominator's retained size is everything it keeps alive). Read it as a \
         concentration curve: if **Top 1** is already high, one object is the leak and \
         freeing it reclaims most of the heap; if the share only climbs as you widen to \
         **Top 10** / **Top 100**, the leak is spread across many peers (e.g. a big cache \
         or collection of similar objects) and no single free helps much._\n\n",
    );
    let mut t = Table::new(&["Scope", "Retained Share"], &[Align::Left, Align::Right]);
    t.row([
        "Top 1 object".into(),
        format!("{:.1}%", rc.top1_bp as f64 / 100.0),
    ]);
    t.row([
        "Top 10 objects".into(),
        format!("{:.1}%", rc.top10_bp as f64 / 100.0),
    ]);
    t.row([
        "Top 100 objects".into(),
        format!("{:.1}%", rc.top100_bp as f64 / 100.0),
    ]);
    t.row([
        "Objects each >=1%".into(),
        fmt_count(rc.num_objects_ge_1pct),
    ]);
    t.render(out);
    out.push('\n');
}

/// Dominator-Depth Distribution (B2): objects per idom-hop below a GC root.
/// Rendered as a standalone section near the end of the report.
pub(crate) fn render_dominator_depth(o: &SystemOverview, out: &mut String) {
    use crate::md::{Align, Table};
    let Some(stats) = depth_stats(&o.dominator_depth_histogram) else {
        return;
    };
    const DEPTH_CAP: usize = 50;
    out.push_str("## Dominator-Depth Distribution\n\n");
    out.push_str(DEPTH_DIST_CAPTION);
    out.push_str(&depth_summary_line(&stats));
    let total = stats.rows.len();
    let shown = total.min(DEPTH_CAP);
    let mut t = Table::new(
        &["Depth", "Objects", "% Objects", "Cumulative %"],
        &[Align::Right, Align::Right, Align::Right, Align::Right],
    );
    for &(depth, objects, pct, cum) in stats.rows.iter().take(shown) {
        t.row([
            depth.to_string(),
            fmt_count(objects),
            fmt_pct(pct),
            fmt_pct(cum),
        ]);
    }
    t.render(out);
    if total > shown {
        out.push_str(&format!(
            "\n_… (+{} deeper buckets in JSON)_\n",
            total - shown
        ));
    }
    out.push('\n');
}

/// Glossary of the memory-analysis terms used throughout the report. Placed last
/// so a reader who hits an unfamiliar term (retained heap, dominator, GC root, …)
/// has one definitive place to look. Shared by both Markdown renderers so the
/// wording stays in lock-step across formats.
pub(crate) fn render_glossary(out: &mut String) {
    out.push_str(GLOSSARY);
}

/// The glossary body. A single source of truth for both the plain-Markdown and
/// the ASCII-graph renderers.
pub(crate) const GLOSSARY: &str = "\
## Glossary

_Definitions for the terms used above._

- **Shallow size**: the memory an object occupies by itself, meaning its header
  plus its own fields (and, for an array, its elements). It does *not* include the
  objects it points to.
- **Retained heap (retained size)**: the total memory that would be freed if this
  object were garbage-collected, meaning its own shallow size plus everything
  reachable *only* through it. This is the number that answers \"how much does
  freeing this actually reclaim?\" and it is the basis for every percentage in this
  report. See [dominator (graph theory)](https://en.wikipedia.org/wiki/Dominator_(graph_theory)).
- **Reachable heap**: all objects the [garbage collector](https://en.wikipedia.org/wiki/Garbage_collection_(computer_science)) can still
  reach from a GC root. Anything unreachable is already collectible and is excluded
  from the totals here.
- **GC root**: an object the JVM keeps alive unconditionally, such as live thread
  stacks (local variables), static fields of loaded classes,
  [JNI](https://en.wikipedia.org/wiki/Java_Native_Interface) references, and
  similar. Every retained-size chain ends at a GC root.
- **Dominator**: object *A* dominates object *B* if every path from a GC root to
  *B* passes through *A*. In other words, if *A* were freed, *B* would become
  unreachable too. An object's retained heap is exactly the set of objects it
  dominates. See [dominator (graph theory)](https://en.wikipedia.org/wiki/Dominator_(graph_theory)).
- **Dominator tree**: the tree formed by linking each object to its immediate
  dominator. Retained sizes are computed by summing shallow sizes up this tree.
- **Top-level dominator**: an object whose immediate dominator is a GC root, so it
  sits at the top of the dominator tree. The \"Biggest Objects\" and \"Retention
  Concentration\" views rank these.
- **Dominator depth**: how many dominator-tree hops an object sits below a GC root.
  Shallow depth means most objects are held close to a root; deep depth means
  retention flows through long chains (nested collections, linked lists).
- **Accumulation point**: a single object (often a collection, cache, or map) that
  dominates a large number of instances of the *same* class, meaning where a
  [memory leak](https://en.wikipedia.org/wiki/Memory_leak) accumulates.
- **Class loader**: the JVM component that defined a class. The same class name
  loaded by two different [class loaders](https://en.wikipedia.org/wiki/Java_Classloader)
  is two distinct classes in the heap, so heap is attributed per (class, loader)
  pair.
- **Referent**: the object that a reference field points *to*. A
  [`WeakReference`](https://en.wikipedia.org/wiki/Weak_reference), for example, has
  a referent it does not keep alive.
- **Instance vs. class**: an *instance* is one object; a *class* row aggregates
  every instance of that type. \"Largest\" in the histogram is the shallow size of
  the single biggest instance of a class.
";

/// Render the "System Overview" section (plain Markdown): scalars, GC-roots and
/// heap-composition breakdowns, and the full class histogram. Byte-exact-tested.
fn render_system_overview(o: &SystemOverview, out: &mut String) {
    use crate::md::{Align, Table};
    out.push_str("## System Overview\n\n");
    out.push_str("_Reachable-heap totals and the largest classes by retained heap._\n\n");
    out.push_str("### Heap Summary\n\n");
    let mut summary = Table::new(&["Property", "Value"], &[Align::Left, Align::Left]);
    summary.row(["HPROF format".into(), o.format.clone()]);
    summary.row(["File size".into(), format_bytes(o.file_size)]);
    summary.row([
        "Identifier size".into(),
        format!("{}-bit", o.identifier_size_bits),
    ]);
    if let Some(coops) = o.compressed_oops {
        summary.row([
            "Compressed OOPs".into(),
            if coops { "yes" } else { "no" }.into(),
        ]);
    }
    if let Some(ms) = o.dump_creation {
        summary.row(["Dump created".into(), format_epoch_ms(ms)]);
    }
    if let Some(ver) = &o.jvm_version {
        summary.row(["JVM version".into(), ver.clone()]);
    }
    summary.row(["Total objects".into(), fmt_count(o.total_objects)]);
    summary.row(["Total shallow heap".into(), format_bytes(o.total_shallow)]);
    summary.row(["GC roots".into(), fmt_count(o.gc_roots)]);
    summary.row(["Classes loaded".into(), fmt_count(o.classes_loaded)]);
    summary.row(["Class loaders".into(), fmt_count(o.classloaders_loaded)]);
    if o.unreachable_count > 0 {
        summary.row([
            "Unreachable objects (excluded)".into(),
            format!(
                "{} ({})",
                fmt_count(o.unreachable_count),
                format_bytes(o.unreachable_shallow),
            ),
        ]);
    }
    if o.heap_fragmentation_ratio > 0.0 {
        summary.row([
            "Heap fragmentation".into(),
            format!("{:.1}%", o.heap_fragmentation_ratio * 100.0),
        ]);
    }
    if o.top_class_concentration_bp > 0 {
        summary.row([
            "Top-class retained concentration".into(),
            format!("{:.1}%", o.top_class_concentration_bp as f64 / 100.0),
        ]);
    }
    summary.render(out);
    out.push('\n');

    // Class-loader labels (additive; does not restructure the tables above).
    // List the distinct non-boot loader labels seen across histogram rows, in
    // first-seen order, capped for readability. Skips the `<boot>` label.
    {
        let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
        let mut labels: Vec<&str> = Vec::new();
        for row in &o.histogram {
            if let Some(lbl) = row.loader_label.as_deref() {
                if lbl != "<boot>" && seen.insert(lbl) {
                    labels.push(lbl);
                }
            }
        }
        if !labels.is_empty() {
            const CAP: usize = 8;
            let shown = labels.len().min(CAP);
            let mut line = labels[..shown].join(", ");
            if labels.len() > CAP {
                line.push_str(&format!(", … (+{} more)", labels.len() - CAP));
            }
            out.push_str(&format!("- **Class loaders (labels):** {line}\n\n"));
        }
    }

    // System properties (additive; captured from java.lang.System.props). Table
    // capped for readability; the full sorted list lives in JSON. Values are
    // truncated to keep rows scannable.
    if !o.system_properties.is_empty() {
        const CAP: usize = 40;
        const VAL_MAX: usize = 120;
        out.push_str("### System Properties\n\n");
        let shown = o.system_properties.len().min(CAP);
        let mut t = Table::new(&["Property", "Value"], &[Align::Left, Align::Left]);
        for p in &o.system_properties[..shown] {
            let mut v = p.value.replace('\n', " ").replace('|', "\\|");
            if v.chars().count() > VAL_MAX {
                let truncated: String = v.chars().take(VAL_MAX).collect();
                v = format!("{truncated}…");
            }
            t.row([p.key.clone(), v]);
        }
        t.render(out);
        if o.system_properties.len() > CAP {
            out.push_str(&format!(
                "\n_… (+{} more properties in JSON)_\n",
                o.system_properties.len() - CAP
            ));
        }
        out.push('\n');
    }

    // (a single-type breakdown restates the "GC roots" scalar above).
    if o.gc_roots_by_type.len() > 1 {
        out.push_str("### GC Roots by Type\n\n");
        let mut t = Table::new(&["Root Type", "Count"], &[Align::Left, Align::Right]);
        for row in &o.gc_roots_by_type {
            t.row([row.root_type.clone(), fmt_count(row.count)]);
        }
        t.render(out);
        out.push('\n');
    }

    // Heap composition by kind: worth a table only when >1 kind present
    // (a single-kind heap just restates "Total objects").
    if o.heap_composition.by_kind.len() > 1 {
        out.push_str("### Heap Composition\n\n");
        let mut t = Table::new(
            &["Kind", "Objects", "Shallow Heap"],
            &[Align::Left, Align::Right, Align::Right],
        );
        for k in &o.heap_composition.by_kind {
            t.row([
                k.kind.clone(),
                fmt_count(k.objects),
                format_bytes(k.shallow_heap),
            ]);
        }
        t.render(out);
        out.push('\n');
    }

    render_record_census(out, &o.record_census);
    render_duplicate_strings(out, &o.duplicate_strings, false);

    out.push_str("### Class Histogram (by Retained Heap)\n\n");
    out.push_str(
        "_Top 50 classes ranked by retained heap; the full list is in the JSON output._\n\n",
    );
    let mut hist = Table::new(
        &[
            "#",
            "Class",
            "Instances",
            "Shallow Heap",
            "Largest",
            "Retained Heap",
        ],
        &[
            Align::Right,
            Align::Left,
            Align::Right,
            Align::Right,
            Align::Right,
            Align::Right,
        ],
    );
    // The model carries the FULL histogram; the Markdown view shows the top 50
    // rows for readability. The complete data lives in the JSON output.
    // Retained heap uses human-readable byte units (matching every other
    // retained/shallow column) so the scale is scannable at a glance.
    // "Largest" is the shallow size of the single biggest instance of the class.
    for (rank, row) in o.histogram.iter().take(50).enumerate() {
        hist.row([
            (rank + 1).to_string(),
            format!("`{}`", row.pretty_class),
            fmt_count(row.instances),
            format_bytes(row.shallow),
            format_bytes(row.max_instance_shallow),
            format_bytes(row.retained),
        ]);
    }
    hist.render(out);
    out.push('\n');

    // Class Loaders (F2): per-loader rollup, top-N by retained heap.
    if !o.loader_rollup.is_empty() {
        out.push_str("### Class Loaders\n\n");
        out.push_str(
            "_Classes grouped by the loader that defined them; many loaders each holding heap \
             can signal a class-loader leak._\n\n",
        );
        let mut t = Table::new(
            &[
                "Loader",
                "Classes",
                "Instances",
                "Shallow Heap",
                "Retained Heap",
            ],
            &[
                Align::Left,
                Align::Right,
                Align::Right,
                Align::Right,
                Align::Right,
            ],
        );
        for r in &o.loader_rollup {
            t.row([
                r.loader_label.clone().unwrap_or_else(|| "<unknown>".into()),
                fmt_count(r.class_count),
                fmt_count(r.instances),
                format_bytes(r.shallow),
                format_bytes(r.retained),
            ]);
        }
        t.render(out);
        out.push('\n');
    }

    // Duplicate Classes (F2): class names loaded under more than one loader.
    if !o.duplicate_classes.is_empty() {
        out.push_str("### Duplicate Classes\n\n");
        out.push_str(
            "_Class names loaded by more than one class loader — a classic class-loader-leak \
             signature (the same class re-loaded repeatedly)._\n\n",
        );
        let mut t = Table::new(
            &["Class", "#Loaders", "Instances", "Retained Heap"],
            &[Align::Left, Align::Right, Align::Right, Align::Right],
        );
        for d in &o.duplicate_classes {
            t.row([
                format!("`{}`", d.pretty_class),
                fmt_count(d.loader_count),
                fmt_count(d.total_instances),
                format_bytes(d.total_retained),
            ]);
        }
        t.render(out);
        out.push('\n');

        // Per-loader drill-down: which loader holds the most of each duplicate.
        for d in &o.duplicate_classes {
            if d.per_loader.is_empty() {
                continue;
            }
            out.push_str(&format!("**`{}`** — per loader:\n\n", d.pretty_class));
            let mut lt = Table::new(
                &["Loader", "Instances", "Shallow", "Retained Heap"],
                &[Align::Left, Align::Right, Align::Right, Align::Right],
            );
            // When two loaders share a display label (distinct instances of the
            // same loader class — the leak signature), append the loader id so
            // the rows are distinguishable.
            let ambiguous: std::collections::HashSet<&str> = {
                let mut seen = std::collections::HashSet::new();
                let mut dup = std::collections::HashSet::new();
                for pl in &d.per_loader {
                    if !seen.insert(pl.loader_label.as_str()) {
                        dup.insert(pl.loader_label.as_str());
                    }
                }
                dup
            };
            for pl in &d.per_loader {
                let label = if ambiguous.contains(pl.loader_label.as_str()) {
                    format!("`{}` @{:#x}", pl.loader_label, pl.loader_id)
                } else {
                    format!("`{}`", pl.loader_label)
                };
                lt.row([
                    label,
                    fmt_count(pl.instances),
                    format_bytes(pl.shallow),
                    format_bytes(pl.retained),
                ]);
            }
            lt.render(out);
            out.push('\n');
        }
    }
}

/// Render the "Leak Suspects" section (plain Markdown): per-suspect footprint,
/// accumulation-path, and dominated-children detail. The root-path and
/// dominator-subtree sub-sections are emitted when their fields are present
/// (root path only for single suspects; subtree only when an accumulation point
/// exists). Byte-exact-tested.
fn render_leak_suspects(l: &LeakSuspects, out: &mut String) {
    out.push_str("## Leak Suspects\n\n");

    if l.suspects.is_empty() {
        out.push_str("No single object or class group exceeds the threshold.\n\n");
        return;
    }

    out.push_str(
        "_Objects and class groups whose retained heap is large enough to be a likely OOM cause, ranked by retained heap._\n\n",
    );

    for (rank, s) in l.suspects.iter().enumerate() {
        let pct = if l.total_shallow > 0 {
            s.retained as f64 / l.total_shallow as f64 * 100.0
        } else {
            0.0
        };

        out.push_str(&format!(
            "### {}. `{}` — retains {} ({:.1}% of reachable heap)\n\n",
            rank + 1,
            s.pretty_class,
            format_bytes(s.retained),
            pct,
        ));

        // What the suspect is: a single object vs a class group.
        if s.is_single {
            out.push_str(&format!(
                "One `{}` object (shallow {}) dominates this retained heap.\n\n",
                s.pretty_class,
                format_bytes(s.shallow),
            ));
        } else {
            out.push_str(&format!(
                "{} instances of `{}` together retain this heap (combined shallow {}).\n\n",
                fmt_count(s.instance_count),
                s.pretty_class,
                format_bytes(s.shallow),
            ));
        }

        // Accumulation point: where the retained heap actually piles up.
        if s.is_single {
            if !s.root_type_label.is_empty() {
                out.push_str(&format!("Held by a **{}** GC root.\n\n", s.root_type_label));
            }
            match (
                &s.accumulation_class,
                s.accumulation_obj_1based,
                s.accumulation_retained,
            ) {
                (Some(ac), Some(_), Some(ret)) => {
                    if s.path.len() <= 1 {
                        out.push_str(&format!(
                            "This object is itself the accumulation point (retained {}).\n\n",
                            format_bytes(ret),
                        ));
                    } else {
                        out.push_str(&format!(
                            "Retained heap accumulates at `{}` (retained {}).\n\n",
                            ac,
                            format_bytes(ret),
                        ));
                    }
                }
                _ => {
                    out.push_str(
                        "No single accumulation point was found within the search depth.\n\n",
                    );
                }
            }
        }

        // Accumulated objects (immediately dominated by the accumulation point).
        if !s.dominated.is_empty() {
            use crate::md::{Align, Table};
            if s.dominated_total_count > s.dominated_shown {
                out.push_str(&format!(
                    "_Directly dominates {} objects (showing top {})._\n\n",
                    fmt_count(s.dominated_total_count),
                    fmt_count(s.dominated_shown),
                ));
            } else if s.dominated_total_count > 0 {
                out.push_str(&format!(
                    "_Directly dominates {} objects._\n\n",
                    fmt_count(s.dominated_total_count),
                ));
            }
            out.push_str(&format!(
                "**Accumulated objects (top {} by retained heap):**\n\n",
                s.dominated.len(),
            ));
            let mut t = Table::new(
                &["Class", "Shallow", "Retained"],
                &[Align::Left, Align::Right, Align::Right],
            );
            for row in &s.dominated {
                t.row([
                    format!("`{}`", row.display_class),
                    format_bytes(row.shallow),
                    format_bytes(row.retained),
                ]);
            }
            t.render(out);
            out.push('\n');
        }

        // By-class histogram of the accumulated objects.
        if !s.dominated_by_class.is_empty() {
            use crate::md::{Align, Table};
            out.push_str("**Accumulated objects by class:**\n\n");
            let mut t = Table::new(
                &["Class", "Objects", "Shallow", "Retained"],
                &[Align::Left, Align::Right, Align::Right, Align::Right],
            );
            for row in &s.dominated_by_class {
                t.row([
                    format!("`{}`", row.pretty_class),
                    fmt_count(row.instances),
                    format_bytes(row.shallow),
                    format_bytes(row.retained),
                ]);
            }
            t.render(out);
            out.push('\n');
        }

        // Dominator chain to a GC root (single suspects only).
        if let Some(path) = &s.root_path {
            render_root_path(path, out);
        }
        // Full multi-level dominator subtree at the accumulation point.
        if let Some(tree) = &s.dominator_tree {
            render_dom_tree_plain(tree, out);
        }
        // Merged shortest paths to GC roots (group suspects only).
        if !s.is_single {
            if let Some(root) = &s.merged_paths {
                render_merged_paths_plain(root, out);
            }
        }
    }
}

/// Render the "Top Consumers" section (plain Markdown): biggest objects,
/// biggest classes, and the pruned package tree. Byte-exact-tested.
fn render_top_consumers(t: &TopConsumers, total_shallow: u64, out: &mut String) {
    use crate::md::{Align, Table};
    out.push_str("## Top Consumers\n\n");
    out.push_str("### Biggest Objects (Top-Level Dominators)\n\n");
    out.push_str(
        "_Individual objects retaining the most heap; `% Heap` is the share of total reachable heap._\n\n",
    );
    let mut objs = Table::new(
        &["#", "Class", "Shallow", "Retained", "% Heap"],
        &[
            Align::Right,
            Align::Left,
            Align::Right,
            Align::Right,
            Align::Right,
        ],
    );
    for (rank, row) in t.biggest_objects.iter().enumerate() {
        let pct = if total_shallow > 0 {
            row.retained as f64 / total_shallow as f64 * 100.0
        } else {
            0.0
        };
        objs.row([
            (rank + 1).to_string(),
            format!("`{}`", row.display_class),
            format_bytes(row.shallow),
            format_bytes(row.retained),
            format!("{:.1}%", pct),
        ]);
    }
    objs.render(out);
    out.push('\n');

    out.push_str("### Biggest Classes by Retained Heap\n\n");
    out.push_str("_Classes whose instances together retain the most heap._\n\n");
    let mut classes = Table::new(
        &["#", "Class", "Instances", "Retained Heap"],
        &[Align::Right, Align::Left, Align::Right, Align::Right],
    );
    for (rank, row) in t.biggest_classes.iter().enumerate() {
        classes.row([
            (rank + 1).to_string(),
            format!("`{}`", row.pretty_class),
            fmt_count(row.instances),
            format_bytes(row.retained),
        ]);
    }
    classes.render(out);
    out.push('\n');

    // Top-Dominator Size Distribution (basic stats + compact bucket table; the
    // md-graphs variant adds a sparkline and bar column).
    if t.size_distribution.count > 0 {
        let d = &t.size_distribution;
        out.push_str("### Top-Dominator Size Distribution\n\n");
        out.push_str(&format!(
            "_Retained-size spread across all {} top-level dominators (the biggest memory contributors)._\n\n",
            d.count
        ));
        out.push_str(&format!("- Dominators: {}\n", fmt_count(d.count)));
        out.push_str(&format!(
            "- Smallest / largest retained: {} / {}\n",
            format_bytes(d.min),
            format_bytes(d.max)
        ));
        out.push_str(&format!("- Median retained: {}\n", format_bytes(d.median)));
        out.push_str(&format!(
            "- Total retained (top-level): {}\n\n",
            format_bytes(d.total)
        ));
        let mut buckets = Table::new(&["Size ≤", "Count"], &[Align::Right, Align::Right]);
        for b in &d.buckets {
            buckets.row([format_bytes(b.upper_bytes), fmt_count(b.count)]);
        }
        buckets.render(out);
        out.push('\n');
    }

    out.push_str("### Biggest Packages by Retained Heap\n\n");
    if t.biggest_packages.children.is_empty() {
        out.push_str("_No package retains more than 1% of the total retained heap._\n");
        out.push('\n');
        return;
    }
    out.push_str(
        "_Retained heap aggregated by package prefix (rows retaining <1% of the total are pruned)._\n\n",
    );
    let mut pkgs = Table::new(
        &["Package", "Objects", "Shallow", "Retained"],
        &[Align::Left, Align::Right, Align::Right, Align::Right],
    );
    // Pre-order DFS; the displayed name is the full dotted path accumulated
    // down from the root, so each row is self-describing (no tree-drawing chars).
    fn emit_node(node: &PackageNode, prefix: &str, pkgs: &mut Table) {
        let full = if prefix.is_empty() {
            node.name.clone()
        } else {
            format!("{}.{}", prefix, node.name)
        };
        pkgs.row([
            format!("`{}`", full),
            fmt_count(node.top_dominator_count),
            format_bytes(node.shallow_heap),
            format_bytes(node.retained_heap),
        ]);
        for child in &node.children {
            emit_node(child, &full, pkgs);
        }
    }
    // Skip the synthetic root (name ""); start emitting at its children.
    for child in &t.biggest_packages.children {
        emit_node(child, "", &mut pkgs);
    }
    pkgs.render(out);
    out.push('\n');
}

/// Render the "Threads" section: an Eclipse-MAT-style Thread Overview table
/// (always-on thread properties) followed by each thread's call stack, with a
/// significant-frames/locals interleave when locals were sampled. Threads
/// without any frames are already dropped upstream; an empty section prints a
/// placeholder so the heading is still self-describing. `graphs` adds a
/// proportional retained-heap bar column to the overview table.
pub(crate) fn render_threads(t: &ThreadOverview, graphs: bool, out: &mut String) {
    use crate::md::{Align, Table};
    out.push_str("## Threads\n\n");
    if t.threads.is_empty() {
        out.push_str("_No thread call stacks were recorded in this dump._\n\n");
        return;
    }

    // ── Thread Overview table (always-on properties) ────────────────────────
    out.push_str("### Thread Overview\n\n");
    out.push_str(
        "_One row per resolved thread; columns mirror Eclipse MAT's Thread Overview._\n\n",
    );
    let retained_max = t.threads.iter().map(|th| th.retained).max().unwrap_or(0);
    let mut headers: Vec<&str> = vec![
        "Name",
        "Shallow",
        "Retained",
        "Max. Locals' Retained",
        "Context Class Loader",
        "Daemon",
        "Priority",
        "State",
    ];
    let mut aligns = vec![
        Align::Left,
        Align::Right,
        Align::Right,
        Align::Right,
        Align::Left,
        Align::Left,
        Align::Right,
        Align::Left,
    ];
    if graphs {
        headers.push("");
        aligns.push(Align::Left);
    }
    let mut tbl = Table::new(&headers, &aligns);
    for th in &t.threads {
        let name = th
            .name
            .clone()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| format!("<thread {}>", th.thread_serial));
        let ctx = th.context_class_loader.as_deref().unwrap_or("—");
        let name_link = format!("[{}](#thread-{})", name, th.thread_serial);
        let mut row = vec![
            name_link,
            format_bytes(th.shallow),
            format_bytes(th.retained),
            format_bytes(th.max_local_retained),
            format!("`{ctx}`"),
            if th.is_daemon { "yes" } else { "no" }.into(),
            th.priority.to_string(),
            if th.thread_state.is_empty() {
                "—".into()
            } else {
                th.thread_state.clone()
            },
        ];
        if graphs {
            row.push(crate::md::bar(
                th.retained,
                retained_max,
                render_graphs::GRAPH_BAR_WIDTH,
            ));
        }
        tbl.row(row);
    }
    tbl.render(out);
    out.push('\n');

    // ── Per-thread call stacks + significant-frame interleave ───────────────
    for th in &t.threads {
        let class = th.class_name.as_deref().unwrap_or("<unresolved>");
        out.push_str(&format!("<a id=\"thread-{}\"></a>\n\n", th.thread_serial));
        match &th.name {
            Some(name) if !name.is_empty() => out.push_str(&format!(
                "### Thread {} \"{}\" ({})\n\n",
                th.thread_serial, name, class
            )),
            _ => out.push_str(&format!("### Thread {} ({})\n\n", th.thread_serial, class)),
        }
        if th.local_root_count > 0 {
            out.push_str(&format!(
                "_Local roots: {}._\n\n",
                fmt_count(th.local_root_count)
            ));
        }
        // A bounded table of this thread's local root objects (empty for
        // threads with no resolved locals ⇒ nothing emitted).
        if let Some(objs) = &th.local_objects {
            render_thread_locals(objs, out);
        }
        // Significant-frames interleave (frames with their retained locals),
        // when locals were sampled; otherwise the plain frame list.
        if !th.significant_frames.is_empty() {
            for sf in &th.significant_frames {
                out.push_str(&format!("- `{}`\n", sf.frame));
                for loc in &sf.locals {
                    out.push_str(&format!(
                        "  - `{}` retains {} ({:.1}%)\n",
                        loc.display_class,
                        format_bytes(loc.retained),
                        loc.pct
                    ));
                }
            }
        } else {
            for frame in &th.frames {
                out.push_str(&format!("- `{frame}`\n"));
            }
        }
        out.push('\n');
    }
}

/// A small table of a thread's local root objects. Emits nothing for an empty
/// list so a thread with no resolved locals adds no clutter. Shared by plain md
/// and md-graphs (no bars).
fn render_thread_locals(objs: &[ThreadLocalObj], out: &mut String) {
    if objs.is_empty() {
        return;
    }
    use crate::md::{Align, Table};
    out.push_str("**Local root objects:**\n\n");
    let mut t = Table::new(
        &["Object", "Shallow", "Retained"],
        &[Align::Left, Align::Right, Align::Right],
    );
    for o in objs {
        t.row([
            format!("`{}`", o.display_class),
            format_bytes(o.shallow),
            format_bytes(o.retained),
        ]);
    }
    t.render(out);
    out.push('\n');
}

/// Render the "Top Components" section: retained heap grouped by class loader
/// (component), mirroring Eclipse MAT's Top Components view. Each row lists the
/// component's retained heap, its share of total reachable retained heap, and
/// its top classes inlined. `graphs` adds a proportional retained bar column.
/// Shared by plain md and md-graphs.
pub(crate) fn render_top_components(tc: &TopComponents, graphs: bool, out: &mut String) {
    use crate::md::{Align, Table};
    out.push_str("## Top Components\n\n");
    if tc.components.is_empty() {
        out.push_str("_No class-loader components were resolved in this dump._\n\n");
        return;
    }
    out.push_str(
        "_Retained heap grouped by class loader (component); `% Heap` is the share of total reachable heap._\n\n",
    );
    let retained_max = tc.components.iter().map(|c| c.retained).max().unwrap_or(0);
    let mut headers: Vec<&str> = vec!["Component", "Retained", "% Heap", "Top classes"];
    let mut aligns = vec![Align::Left, Align::Right, Align::Right, Align::Left];
    if graphs {
        headers.push("");
        aligns.push(Align::Left);
    }
    let mut tbl = Table::new(&headers, &aligns);
    for c in &tc.components {
        let top = c
            .top_classes
            .iter()
            .map(|cc| format!("`{}` ({})", cc.pretty_class, format_bytes(cc.retained)))
            .collect::<Vec<_>>()
            .join(", ");
        let mut row = vec![
            format!("`{}`", c.loader_label),
            format_bytes(c.retained),
            format!("{:.1}%", c.pct),
            top,
        ];
        if graphs {
            row.push(crate::md::bar(
                c.retained,
                retained_max,
                render_graphs::GRAPH_BAR_WIDTH,
            ));
        }
        tbl.row(row);
    }
    tbl.render(out);
    out.push('\n');
}

/// Render the always-on "Arrays by Size" section: two power-of-two length
/// histograms (object arrays, primitive arrays) with object counts + shallow
/// bytes, plus a zero-length tally. Shared by plain md and md-graphs; when
/// `graphs` is set, an extra proportional bar column is appended on Objects.
/// Emits the heading + a fallback italic line even when empty so the document
/// structure stays stable.
pub(crate) fn render_arrays_by_size(a: &ArraysBySize, graphs: bool, out: &mut String) {
    use crate::md::{bar, Align, Table};
    out.push_str("## Arrays by Size\n\n");
    if a.obj_array_buckets.is_empty() && a.prim_array_buckets.is_empty() && a.zero_length_count == 0
    {
        out.push_str("*No arrays found.*\n\n");
        return;
    }
    out.push_str(
        "_Array-length distribution bucketed by power-of-two element length; \
         `Max length` is the inclusive upper bound of each bucket._\n\n",
    );

    let render_table = |title: &str, buckets: &[SizeHistogramBucket], out: &mut String| {
        out.push_str(&format!("### {title}\n\n"));
        if buckets.is_empty() {
            out.push_str("_None._\n\n");
            return;
        }
        let obj_max = buckets.iter().map(|b| b.objects).max().unwrap_or(0);
        let mut headers: Vec<&str> = vec!["Max length", "Objects", "Shallow"];
        let mut aligns = vec![Align::Right, Align::Right, Align::Right];
        if graphs {
            headers.push("");
            aligns.push(Align::Left);
        }
        let mut t = Table::new(&headers, &aligns);
        for b in buckets {
            let mut row = vec![
                format!("≤ {}", fmt_count(b.upper_len)),
                fmt_count(b.objects),
                format_bytes(b.shallow),
            ];
            if graphs {
                row.push(bar(b.objects, obj_max, render_graphs::GRAPH_BAR_WIDTH));
            }
            t.row(row);
        }
        t.render(out);
        out.push('\n');
    };

    render_table("Object arrays", &a.obj_array_buckets, out);
    render_table("Primitive arrays", &a.prim_array_buckets, out);
    out.push_str(&format!(
        "Zero-length arrays: {}\n\n",
        fmt_count(a.zero_length_count)
    ));
}

/// Format a `FillRatioBucket`'s label as a percent range from basis points
/// (0..=10000), e.g. `0–10%` (en-dash), matching the range style used
/// elsewhere in the report.
fn fill_ratio_label(b: &FillRatioBucket) -> String {
    let lo = b.lower_ratio_bp as f64 / 100.0;
    let hi = b.upper_ratio_bp as f64 / 100.0;
    format!("{lo:.0}–{hi:.0}%")
}

/// Render the always-on "Collections" section: five collection/array sub-views
/// (fill ratio, size histogram, object-array fill ratio, map collision ratio,
/// and constant primitive arrays). Shared by plain md and md-graphs; when
/// `graphs` is set, an extra proportional bar column is appended on the object
/// count of each table. Emits the heading + fallback italic lines even when
/// empty so the document structure stays stable.
/// Render a fill-ratio bucket table (`Collection Fill Ratio`, `Array Fill
/// Ratio`, `Map Collision Ratio`). `count_header` names the object column
/// (e.g. "Collections"); `with_wasted` adds the Wasted bytes column. When
/// `graphs` is set a proportional bar column on objects is appended.
fn render_fill_ratio_table(
    buckets: &[FillRatioBucket],
    ratio_header: &str,
    count_header: &str,
    with_wasted: bool,
    graphs: bool,
    out: &mut String,
) {
    use crate::md::{bar, Align, Table};
    if buckets.is_empty() {
        out.push_str("_None._\n\n");
        return;
    }
    let obj_max = buckets.iter().map(|b| b.objects).max().unwrap_or(0);
    let mut headers: Vec<&str> = vec![ratio_header, count_header, "Shallow"];
    let mut aligns = vec![Align::Right, Align::Right, Align::Right];
    if with_wasted {
        headers.push("Wasted");
        aligns.push(Align::Right);
    }
    if graphs {
        headers.push("");
        aligns.push(Align::Left);
    }
    let mut t = Table::new(&headers, &aligns);
    for b in buckets {
        let mut row = vec![
            fill_ratio_label(b),
            fmt_count(b.objects),
            format_bytes(b.shallow),
        ];
        if with_wasted {
            row.push(format_bytes(b.wasted));
        }
        if graphs {
            row.push(bar(b.objects, obj_max, render_graphs::GRAPH_BAR_WIDTH));
        }
        t.row(row);
    }
    t.render(out);
    out.push('\n');
}

pub(crate) fn render_collections(c: &CollectionsAnalysis, graphs: bool, out: &mut String) {
    use crate::md::{bar, Align, Table};
    out.push_str("## Collections\n\n");
    out.push_str(
        "_Collection and array occupancy: how full collections are, how big they get, \
         and constant primitive arrays._\n\n",
    );

    // ── Collections by Kind ──────────────────────────────────────────────────
    out.push_str("### Collections by Kind\n\n");
    if c.kind_summary.kinds.is_empty() {
        out.push_str("_None._\n\n");
    } else {
        let elem_max = c
            .kind_summary
            .kinds
            .iter()
            .map(|s| s.total_elements)
            .max()
            .unwrap_or(0);
        let mut headers: Vec<&str> = vec![
            "Kind",
            "Count",
            "Total Elements",
            "Max Elements",
            "Total Shallow",
        ];
        let mut aligns = vec![
            Align::Left,
            Align::Right,
            Align::Right,
            Align::Right,
            Align::Right,
        ];
        if graphs {
            headers.push("");
            aligns.push(Align::Left);
        }
        let mut t = Table::new(&headers, &aligns);
        for s in &c.kind_summary.kinds {
            let mut row = vec![
                s.kind.clone(),
                fmt_count(s.count),
                fmt_count(s.total_elements),
                fmt_count(s.max_elements),
                format_bytes(s.total_shallow),
            ];
            if graphs {
                row.push(bar(
                    s.total_elements,
                    elem_max,
                    render_graphs::GRAPH_BAR_WIDTH,
                ));
            }
            t.row(row);
        }
        t.render(out);
        out.push('\n');
    }

    // ── Collection Fill Ratio ────────────────────────────────────────────────
    out.push_str("### Collection Fill Ratio\n\n");
    out.push_str(&format!(
        "_{} tracked of {} collections._\n\n",
        fmt_count(c.collection_fill_ratio.tracked),
        fmt_count(c.collection_fill_ratio.total),
    ));
    render_fill_ratio_table(
        &c.collection_fill_ratio.buckets,
        "Fill %",
        "Collections",
        true,
        graphs,
        out,
    );

    // ── Collections by Size ──────────────────────────────────────────────────
    out.push_str("### Collections by Size\n\n");
    out.push_str(&format!(
        "_{} tracked; {} empty._\n\n",
        fmt_count(c.collections_by_size.tracked),
        fmt_count(c.collections_by_size.empty_count),
    ));
    if c.collections_by_size.buckets.is_empty() {
        out.push_str("_None._\n\n");
    } else {
        let obj_max = c
            .collections_by_size
            .buckets
            .iter()
            .map(|b| b.objects)
            .max()
            .unwrap_or(0);
        let mut headers: Vec<&str> = vec!["Size ≤", "Collections", "Shallow"];
        let mut aligns = vec![Align::Right, Align::Right, Align::Right];
        if graphs {
            headers.push("");
            aligns.push(Align::Left);
        }
        let mut t = Table::new(&headers, &aligns);
        for b in &c.collections_by_size.buckets {
            let mut row = vec![
                format!("≤ {}", fmt_count(b.upper_len)),
                fmt_count(b.objects),
                format_bytes(b.shallow),
            ];
            if graphs {
                row.push(bar(b.objects, obj_max, render_graphs::GRAPH_BAR_WIDTH));
            }
            t.row(row);
        }
        t.render(out);
        out.push('\n');
    }

    // ── Array Fill Ratio ─────────────────────────────────────────────────────
    out.push_str("### Array Fill Ratio\n\n");
    out.push_str(&format!(
        "_{} tracked object arrays._\n\n",
        fmt_count(c.array_fill_ratio.tracked),
    ));
    render_fill_ratio_table(
        &c.array_fill_ratio.buckets,
        "Fill %",
        "Arrays",
        true,
        graphs,
        out,
    );

    // ── Map Collision Ratio ──────────────────────────────────────────────────
    out.push_str("### Map Collision Ratio\n\n");
    out.push_str(&format!(
        "_{} tracked of {} maps (occupied slots ÷ size; lower is worse)._\n\n",
        fmt_count(c.map_collision_ratio.tracked),
        fmt_count(c.map_collision_ratio.total),
    ));
    render_fill_ratio_table(
        &c.map_collision_ratio.buckets,
        "Load %",
        "Maps",
        false,
        graphs,
        out,
    );

    // ── Constant Primitive Arrays ────────────────────────────────────────────
    out.push_str("### Constant Primitive Arrays\n\n");
    let mut note = String::from("_Primitive arrays whose every element is identical._");
    if c.constant_primitive_arrays.truncated {
        note.push_str(" _(list truncated; remaining groups folded into one row)._");
    }
    out.push_str(&note);
    out.push_str("\n\n");
    if c.constant_primitive_arrays.rows.is_empty() {
        out.push_str("_None._\n\n");
    } else {
        let obj_max = c
            .constant_primitive_arrays
            .rows
            .iter()
            .map(|r| r.objects)
            .max()
            .unwrap_or(0);
        let mut headers: Vec<&str> = vec!["Array class", "Length", "Value", "Objects", "Shallow"];
        let mut aligns = vec![
            Align::Left,
            Align::Right,
            Align::Right,
            Align::Right,
            Align::Right,
        ];
        if graphs {
            headers.push("");
            aligns.push(Align::Left);
        }
        let mut t = Table::new(&headers, &aligns);
        for r in &c.constant_primitive_arrays.rows {
            let mut row = vec![
                format!("`{}`", r.array_class),
                fmt_count(r.length),
                format!("{}", r.value),
                fmt_count(r.objects),
                format_bytes(r.shallow),
            ];
            if graphs {
                row.push(bar(r.objects, obj_max, render_graphs::GRAPH_BAR_WIDTH));
            }
            t.row(row);
        }
        t.render(out);
        out.push('\n');
    }

    // ── Top Arrays ───────────────────────────────────────────────────────────
    render_top_arrays(&c.top_prim_arrays, "primitive", graphs, out);
    render_top_arrays(&c.top_obj_arrays, "object", graphs, out);
}

/// Render the two Top Arrays tables (largest individual arrays + largest array
/// classes by aggregate shallow) for one category. Shared by plain md and
/// md-graphs; when `graphs` is set an extra proportional bar column is appended
/// on Shallow.
fn render_top_arrays(t: &TopArrays, kind: &str, graphs: bool, out: &mut String) {
    use crate::md::{bar, Align, Table};

    out.push_str(&format!("### Top Arrays ({kind})\n\n"));
    out.push_str(&format!(
        "_The largest {kind} arrays by shallow size, individually and aggregated by array class._\n\n"
    ));

    // Largest individual arrays.
    if t.top_individual.is_empty() {
        out.push_str("_None._\n\n");
    } else {
        let sh_max = t
            .top_individual
            .iter()
            .map(|r| r.shallow)
            .max()
            .unwrap_or(0);
        let mut headers: Vec<&str> = vec!["Array class", "Length", "Shallow"];
        let mut aligns = vec![Align::Left, Align::Right, Align::Right];
        if graphs {
            headers.push("");
            aligns.push(Align::Left);
        }
        let mut tbl = Table::new(&headers, &aligns);
        for r in &t.top_individual {
            let mut row = vec![
                format!("`{}`", r.array_class),
                fmt_count(r.length),
                format_bytes(r.shallow),
            ];
            if graphs {
                row.push(bar(r.shallow, sh_max, render_graphs::GRAPH_BAR_WIDTH));
            }
            tbl.row(row);
        }
        tbl.render(out);
        out.push('\n');
    }

    // Largest array classes by aggregate shallow.
    out.push_str(&format!("#### Top Array Classes ({kind})\n\n"));
    if t.top_by_class.is_empty() {
        out.push_str("_None._\n\n");
    } else {
        let sh_max = t.top_by_class.iter().map(|r| r.shallow).max().unwrap_or(0);
        let mut headers: Vec<&str> = vec!["Array class", "Instances", "Shallow"];
        let mut aligns = vec![Align::Left, Align::Right, Align::Right];
        if graphs {
            headers.push("");
            aligns.push(Align::Left);
        }
        let mut tbl = Table::new(&headers, &aligns);
        for r in &t.top_by_class {
            let mut row = vec![
                format!("`{}`", r.array_class),
                fmt_count(r.objects),
                format_bytes(r.shallow),
            ];
            if graphs {
                row.push(bar(r.shallow, sh_max, render_graphs::GRAPH_BAR_WIDTH));
            }
            tbl.row(row);
        }
        tbl.render(out);
        out.push('\n');
    }
}

/// Render the Container Attribution (Class#field) section: which holder
/// `Class#field` points at the most container memory. Two rankings — total
/// across all containers reached through a field, and the single largest
/// container per field. Shared by plain md and md-graphs; when `graphs` is set
/// a proportional bar column is appended on the element counts. Absent
/// entirely when `--collections` was off (`a` is `None`).
pub(crate) fn render_collection_attribution(
    a: &Option<CollectionAttribution>,
    graphs: bool,
    out: &mut String,
) {
    use crate::md::{bar, Align, Table};
    let Some(a) = a else {
        return;
    };

    out.push_str("## Container Attribution (Class#field)\n\n");
    out.push_str(
        "_Which holder Class#field points at the most container memory. Two rankings: total \
         across all containers reached through a field, and the single largest container per \
         field._\n\n",
    );

    // ── Most Overall ─────────────────────────────────────────────────────────
    out.push_str("### Most Overall\n\n");
    if a.most_overall.is_empty() {
        out.push_str("_None._\n\n");
    } else {
        let el_max = a
            .most_overall
            .iter()
            .map(|r| r.total_elements)
            .max()
            .unwrap_or(0);
        let mut headers: Vec<&str> = vec![
            "Class#field",
            "Kind",
            "Containers",
            "Holder Instances",
            "Total Elements",
            "Total Retained",
        ];
        let mut aligns = vec![
            Align::Left,
            Align::Left,
            Align::Right,
            Align::Right,
            Align::Right,
            Align::Right,
        ];
        if graphs {
            headers.push("");
            aligns.push(Align::Left);
        }
        let mut t = Table::new(&headers, &aligns);
        for r in &a.most_overall {
            let mut row = vec![
                format!("`{}#{}`", r.holder_class, r.field),
                r.container_kind.clone(),
                fmt_count(r.container_count),
                fmt_count(r.holder_instances),
                fmt_count(r.total_elements),
                format_bytes(r.total_retained),
            ];
            if graphs {
                row.push(bar(
                    r.total_elements,
                    el_max,
                    render_graphs::GRAPH_BAR_WIDTH,
                ));
            }
            t.row(row);
        }
        t.render(out);
        out.push('\n');
    }

    // ── Biggest Single ───────────────────────────────────────────────────────
    out.push_str("### Biggest Single\n\n");
    if a.biggest_single.is_empty() {
        out.push_str("_None._\n\n");
    } else {
        let el_max = a
            .biggest_single
            .iter()
            .map(|r| r.elements)
            .max()
            .unwrap_or(0);
        let mut headers: Vec<&str> = vec![
            "Class#field",
            "Container Class",
            "Elements",
            "Capacity",
            "Retained",
        ];
        let mut aligns = vec![
            Align::Left,
            Align::Left,
            Align::Right,
            Align::Right,
            Align::Right,
        ];
        if graphs {
            headers.push("");
            aligns.push(Align::Left);
        }
        let mut t = Table::new(&headers, &aligns);
        for r in &a.biggest_single {
            let mut row = vec![
                format!("`{}#{}`", r.holder_class, r.field),
                format!("`{}`", r.container_class),
                fmt_count(r.elements),
                fmt_count(r.capacity),
                format_bytes(r.retained),
            ];
            if graphs {
                row.push(bar(r.elements, el_max, render_graphs::GRAPH_BAR_WIDTH));
            }
            t.row(row);
        }
        t.render(out);
        out.push('\n');
    }

    if a.truncated {
        out.push_str(
            "_Attribution data was truncated (holder-edge or container-record cap hit); \
             rankings are a bounded sample._\n\n",
        );
    }
}
/// referent histograms plus (where present) an approximate only-weakly-retained
/// breakdown. Shared by plain md and md-graphs; when `graphs` is set an extra
/// proportional bar column is appended on Objects. Emits the heading + a
/// fallback line even when no references are present so the structure stays
/// stable.
pub(crate) fn render_references(rf: &ReferencesAnalysis, graphs: bool, out: &mut String) {
    use crate::md::{bar, Align, Table};
    out.push_str("## References\n\n");
    out.push_str("_Soft/weak/phantom reference referents (what they point at)._\n\n");

    if rf.soft.is_none() && rf.weak.is_none() && rf.phantom.is_none() {
        out.push_str("_No soft, weak, or phantom references found._\n\n");
        return;
    }

    let render_class_table = |rows: &[RefStatClassRow], out: &mut String| {
        let obj_max = rows.iter().map(|r| r.objects).max().unwrap_or(0);
        let mut headers: Vec<&str> = vec!["Class", "Objects", "Shallow"];
        let mut aligns = vec![Align::Left, Align::Right, Align::Right];
        if graphs {
            headers.push("");
            aligns.push(Align::Left);
        }
        let mut t = Table::new(&headers, &aligns);
        for r in rows {
            let mut row = vec![
                format!("`{}`", r.pretty_class),
                fmt_count(r.objects),
                format_bytes(r.shallow),
            ];
            if graphs {
                row.push(bar(r.objects, obj_max, render_graphs::GRAPH_BAR_WIDTH));
            }
            t.row(row);
        }
        t.render(out);
        out.push('\n');
    };

    for stats in [&rf.soft, &rf.weak, &rf.phantom].into_iter().flatten() {
        out.push_str(&format!("### {} References\n\n", stats.kind));
        out.push_str(&format!(
            "_{} reference instances._\n\n",
            fmt_count(stats.reference_instances),
        ));
        out.push_str("#### Referent classes\n\n");
        render_class_table(&stats.referent_histogram, out);
        if !stats.only_weakly_retained.is_empty() {
            out.push_str("#### Only-weakly retained _(approximate)_\n\n");
            render_class_table(&stats.only_weakly_retained, out);
        }
    }
}

/// Render the always-on "Unreachable Objects" section: a per-class histogram of
/// objects not dominated by the virtual root (`idom == u32::MAX`), sorted by
/// shallow descending and capped. Shared by plain md and md-graphs; when
/// `graphs` is set, an extra proportional bar column is appended on Objects.
/// Emits the heading + a fallback italic line even when empty so the document
/// structure stays stable.
pub(crate) fn render_unreachable_histogram(o: &SystemOverview, graphs: bool, out: &mut String) {
    use crate::md::{bar, Align, Table};
    out.push_str("## Unreachable Objects\n\n");
    if o.unreachable_histogram.is_empty() {
        out.push_str("*No unreachable objects.*\n\n");
        return;
    }
    out.push_str(&format!(
        "_{} unreachable objects retaining {} shallow (top {} classes by shallow)._\n\n",
        fmt_count(o.unreachable_count),
        format_bytes(o.unreachable_shallow),
        UNREACHABLE_HISTOGRAM_CAP,
    ));
    let obj_max = o
        .unreachable_histogram
        .iter()
        .map(|r| r.objects)
        .max()
        .unwrap_or(0);
    let mut headers: Vec<&str> = vec!["Class", "Objects", "Shallow"];
    let mut aligns = vec![Align::Left, Align::Right, Align::Right];
    if graphs {
        headers.push("");
        aligns.push(Align::Left);
    }
    let mut t = Table::new(&headers, &aligns);
    for r in &o.unreachable_histogram {
        let mut row = vec![
            format!("`{}`", r.pretty_class),
            fmt_count(r.objects),
            format_bytes(r.shallow),
        ];
        if graphs {
            row.push(bar(r.objects, obj_max, render_graphs::GRAPH_BAR_WIDTH));
        }
        t.row(row);
    }
    t.render(out);
    out.push('\n');
}

/// Render the always-on "Dominator Analysis" section: two dominator-tree
/// sub-views. "Big Drops" lists dominators where retained heap concentrates
/// (retained minus the largest single child); "Immediate Dominators" rolls up
/// the immediately-dominated objects by their dominator's class. Shared by plain
/// md and md-graphs; when `graphs` is set, a proportional bar column is appended
/// on Drop (big drops) and on Dominated Shallow (immediate dominators). Emits the
/// headings + fallback italic lines even when empty so the structure stays stable.
pub(crate) fn render_dominator_analysis(d: &DominatorAnalysis, graphs: bool, out: &mut String) {
    use crate::md::{bar, Align, Table};
    out.push_str("## Dominator Analysis\n\n");

    // ---- Big Drops ----
    out.push_str("### Big Drops\n\n");
    let threshold_mb = d.big_drops.threshold as f64 / (1024.0 * 1024.0);
    out.push_str(&format!(
        "_Dominators where retained heap concentrates: retained heap minus the largest single child. Threshold {:.1} MB (1% of reachable shallow)._\n\n",
        threshold_mb,
    ));
    if d.big_drops.rows.is_empty() {
        out.push_str("*No significant drops.*\n\n");
    } else {
        let drop_max = d
            .big_drops
            .rows
            .iter()
            .map(|r| r.drop_bytes)
            .max()
            .unwrap_or(0);
        let mut headers: Vec<&str> = vec![
            "Object",
            "Retained",
            "Largest Child",
            "Child Retained",
            "Drop",
        ];
        let mut aligns = vec![
            Align::Left,
            Align::Right,
            Align::Left,
            Align::Right,
            Align::Right,
        ];
        if graphs {
            headers.push("");
            aligns.push(Align::Left);
        }
        let mut t = Table::new(&headers, &aligns);
        for r in &d.big_drops.rows {
            let child = if r.largest_child_class.is_empty() {
                "—".to_string()
            } else {
                format!("`{}`", r.largest_child_class)
            };
            let mut row = vec![
                format!("`{}`", r.display_class),
                format_bytes(r.retained),
                child,
                format_bytes(r.largest_child_retained),
                format_bytes(r.drop_bytes),
            ];
            if graphs {
                row.push(bar(r.drop_bytes, drop_max, render_graphs::GRAPH_BAR_WIDTH));
            }
            t.row(row);
        }
        t.render(out);
        out.push('\n');
    }

    // ---- Immediate Dominators ----
    out.push_str("### Immediate Dominators\n\n");
    out.push_str(
        "_Objects immediately dominated, rolled up by the dominator's class; \
         a heavy dominated shallow heap under one class flags a retention hub._\n\n",
    );
    if d.immediate_dominators.rows.is_empty() {
        out.push_str("*No immediate dominators.*\n\n");
    } else {
        let shallow_max = d
            .immediate_dominators
            .rows
            .iter()
            .map(|r| r.dominated_shallow)
            .max()
            .unwrap_or(0);
        let mut headers: Vec<&str> = vec![
            "Dominator Class",
            "#Dominators",
            "#Dominated",
            "Dominator Shallow",
            "Dominated Shallow",
        ];
        let mut aligns = vec![
            Align::Left,
            Align::Right,
            Align::Right,
            Align::Right,
            Align::Right,
        ];
        if graphs {
            headers.push("");
            aligns.push(Align::Left);
        }
        let mut t = Table::new(&headers, &aligns);
        for r in &d.immediate_dominators.rows {
            let mut row = vec![
                format!("`{}`", r.dominator_class),
                fmt_count(r.dominator_count),
                fmt_count(r.dominated_count),
                format_bytes(r.dominator_shallow),
                format_bytes(r.dominated_shallow),
            ];
            if graphs {
                row.push(bar(
                    r.dominated_shallow,
                    shallow_max,
                    render_graphs::GRAPH_BAR_WIDTH,
                ));
            }
            t.row(row);
        }
        t.render(out);
        out.push('\n');
    }
}

/// The dominator chain from a
/// suspect (first) up to its GC root (last), as a numbered list. The final step
/// is annotated with the GC-root type when known. Shared verbatim by plain md and
/// md-graphs (a numbered list needs no bars).
pub(crate) fn render_root_path(path: &[RootPathStep], out: &mut String) {
    if path.is_empty() {
        return;
    }
    out.push_str("**Path to GC root (dominator chain):**\n\n");
    let last = path.len() - 1;
    for (i, step) in path.iter().enumerate() {
        let mut line = format!(
            "{}. `{}` ({})",
            i + 1,
            step.display_class,
            format_bytes(step.retained),
        );
        if i == last {
            if let Some(label) = &step.root_type_label {
                line.push_str(&format!(" — GC root: {label}"));
            }
        }
        line.push('\n');
        out.push_str(&line);
    }
    out.push('\n');
}

/// Dominator subtree (plain md): the full multi-level dominator
/// subtree at the accumulation point, as a nested bullet list indented two
/// spaces per level. Uses an explicit stack (the tree can be deep) and emits
/// nodes in the pre-order the `children` Vecs already carry (retained-desc).
fn render_dom_tree_plain(root: &DomTreeNode, out: &mut String) {
    out.push_str("**Dominator subtree:**\n\n");
    // Stack of (node, depth); push children reversed so pre-order pops in order.
    let mut stack: Vec<(&DomTreeNode, usize)> = vec![(root, 0)];
    while let Some((node, depth)) = stack.pop() {
        let indent = "  ".repeat(depth);
        out.push_str(&format!(
            "{}- `{}` (shallow {}, retained {})\n",
            indent,
            node.display_class,
            format_bytes(node.shallow),
            format_bytes(node.retained),
        ));
        for child in node.children.iter().rev() {
            stack.push((child, depth + 1));
        }
    }
    out.push('\n');
}

/// Merged shortest paths to GC roots (plain md): the member objects' dominator
/// chains collapsed into a class-keyed prefix tree, as a nested bullet list
/// indented two spaces per level — the same visual language as
/// `render_dom_tree_plain`. Each line shows the class, how many member chains
/// pass through the node, and the aggregate retained; the terminal GC-root node
/// carries its root-type label.
fn render_merged_paths_plain(root: &MergedPathNode, out: &mut String) {
    out.push_str("#### Merged Paths to GC Roots\n\n");
    // Stack of (node, depth); push children reversed so pre-order pops in order.
    let mut stack: Vec<(&MergedPathNode, usize)> = vec![(root, 0)];
    while let Some((node, depth)) = stack.pop() {
        let indent = "  ".repeat(depth);
        let mut line = format!(
            "{}- `{}` ({} objects, retained {})",
            indent,
            node.display_class,
            fmt_count(node.object_count),
            format_bytes(node.retained),
        );
        if let Some(label) = &node.root_type_label {
            line.push_str(&format!(" — GC root: {label}"));
        }
        line.push('\n');
        out.push_str(&line);
        for child in node.children.iter().rev() {
            stack.push((child, depth + 1));
        }
    }
    out.push('\n');
}
/// the dump carried no allocation stack-trace info. `graphs` adds a proportional
/// bar column (keyed to the max object count) in the md-graphs output.
pub(crate) fn render_alloc_sites(a: &AllocSites, graphs: bool, out: &mut String) {
    out.push_str("## Allocation Sites\n\n");
    if !a.traces_present {
        out.push_str(
            "_Allocation tracking was off in this dump (stack_trace_serial = 0); no allocation sites available._\n\n",
        );
        return;
    }
    use crate::md::{bar, Align, Table};
    let max = a.sites.iter().map(|s| s.object_count).max().unwrap_or(0);
    let mut t = if graphs {
        Table::new(
            &["Stack", "Objects", "Shallow", "Retained", ""],
            &[
                Align::Left,
                Align::Right,
                Align::Right,
                Align::Right,
                Align::Left,
            ],
        )
    } else {
        Table::new(
            &["Stack", "Objects", "Shallow", "Retained"],
            &[Align::Left, Align::Right, Align::Right, Align::Right],
        )
    };
    for site in &a.sites {
        let stack = match site.frames.first() {
            Some(top) => format!("`{top}`"),
            None => format!("serial {}", site.stack_serial),
        };
        if graphs {
            t.row([
                stack,
                fmt_count(site.object_count),
                format_bytes(site.shallow_total),
                format_bytes(site.retained_total),
                bar(site.object_count, max, GRAPH_BAR_WIDTH),
            ]);
        } else {
            t.row([
                stack,
                fmt_count(site.object_count),
                format_bytes(site.shallow_total),
                format_bytes(site.retained_total),
            ]);
        }
    }
    t.render(out);
    out.push('\n');
}

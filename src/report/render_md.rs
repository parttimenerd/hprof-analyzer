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
        "_Raw HPROF record-type composition of the dump (pass-1 counts). \
Useful for diagnosing truncated or unusual dumps (e.g. zero stack frames means \
no allocation-site data; a mismatch between load-class and class-dump counts \
can indicate a partial write). Additive, not parity-compared._\n\n",
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
    use crate::md::{Align, Table, bar};
    out.push_str("### Duplicate Strings (approximate)\n\n");
    let d = match d {
        None => {
            out.push_str("_Duplicate-string analysis not run (pass `--find-duplicates`)._\n\n");
            return;
        }
        Some(d) => d,
    };
    out.push_str(
        "_String values seen more than once — reclaim by normalizing at parse time, \
using `-XX:+UseStringDeduplication` (G1 GC), or sharing a canonical instance per value. \
Deduplication is approximate (64-bit hash; rare collisions possible)._\n\n",
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
        out.push_str("#### `char[]` Waste\n\n");
        out.push_str(
            "_Strings whose `char[]` or `byte[]` backing array is larger than the character \
data — typical of `substring()` retaining a full backing array (Java 6/7 shared-buffer \
semantics) or repeated `StringBuilder.toString()` allocations._\n\n",
        );
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
/// heap, the executive-summary verdict calls it the "likely problem". (The OOM
/// Triage rules use their own copy of this threshold in `triage.rs`.)
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
        "_Point-in-time counts for known Java leak patterns. Non-zero values are not \
always bugs — see the **What to Check** column for how to triage each one._\n\n",
    );
    let mut t = Table::new(
        &["Indicator", "Value", "What to Check"],
        &[Align::Left, Align::Right, Align::Left],
    );
    if li.anonymous_class_count > 0 {
        t.row([
            "Anonymous/generated classes".into(),
            fmt_count(li.anonymous_class_count),
            "High counts signal class-loader leaks (e.g. dynamic proxies accumulating per request). In Top Consumers, filter by `$` to find the biggest offenders.".into(),
        ]);
    }
    if li.thread_local_null_key_count > 0 {
        t.row([
            "`ThreadLocal` null-key entries (cleared referent)".into(),
            fmt_count(li.thread_local_null_key_count),
            "A null key means the `ThreadLocal` object was GC'd while the thread still holds the value — classic leak in thread pools. Call `ThreadLocal.remove()` when done, or use try-finally to guarantee cleanup.".into(),
        ]);
    }
    if li.direct_byte_buffer_capacity_sum > 0 {
        t.row([
            "`DirectByteBuffer` off-heap capacity".into(),
            format_bytes(li.direct_byte_buffer_capacity_sum),
            "Native memory, excluded from JVM heap totals. Check for NIO buffer pools that leak on close, or Netty/gRPC allocators missing a buffer cap.".into(),
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
    if r.truncated_input {
        out.push_str(
            "> **Warning — truncated input:** the heap dump file was incomplete \
             (the gzip stream ended prematurely). This report covers only the \
             objects and classes that were successfully read before the stream \
             ended. Totals, leak suspects, and top consumers may be understated. \
             Re-copy the dump to get a complete analysis.\n\n",
        );
    }
    render_toc(r, &mut out);
    render_executive_summary(r, &mut out);
    render_oom_triage(r, &mut out);
    render_waste_summary(r, &mut out);
    render_system_overview(
        &r.overview,
        r.leak_indicators.direct_byte_buffer_capacity_sum,
        &mut out,
    );
    render_leak_suspects(&r.leaks, &mut out);
    render_top_consumers(&r.top, r.leaks.total_shallow, &mut out);
    render_dominator_analysis(&r.dominator_analysis, false, &mut out);
    render_threads(&r.threads, false, &mut out);
    render_top_components(&r.top_components, false, &mut out);
    render_arrays_by_size(&r.arrays_by_size, false, &mut out);
    render_collections(&r.collections, &r.collection_attribution, false, &mut out);
    render_collection_attribution(&r.collection_attribution, false, &mut out);
    render_fields_by_size(&r.fields_by_size, false, &mut out);
    render_biggest_collections(&r.biggest_collections, false, &mut out);
    render_collection_contents(&r.collection_contents, false, &mut out);
    render_references(&r.references, false, &mut out);
    render_unreachable_histogram(&r.overview, false, &mut out);
    // Allocation sites (always present; `None` only for legacy reports).
    if let Some(a) = &r.alloc_sites {
        render_alloc_sites(a, false, &mut out);
    }
    render_retention_concentration(&r.overview, &mut out);
    render_dominator_depth(&r.overview, &mut out);
    render_leak_indicators(&r.leak_indicators, &mut out);
    render_custom_queries(&r.queries, &mut out);
    render_glossary(&mut out);
    out
}

/// Render the "Custom Queries" section (plain Markdown): one sub-section per
/// user-supplied OQL query — the query name, the OQL text in a fenced block,
/// and either an error line or a result table with a row-count footer. Emits
/// nothing when there are no queries so the document structure is unchanged for
/// the common (no `--query`) case.
pub(crate) fn render_custom_queries(
    queries: &[crate::query::model::QueryResult],
    out: &mut String,
) {
    use std::fmt::Write;
    if queries.is_empty() {
        return;
    }
    let _ = writeln!(out, "\n## Custom Queries\n");
    for q in queries {
        let _ = writeln!(out, "### {}\n", q.name);
        let _ = writeln!(out, "```\n{}\n```\n", q.oql);
        if let Some(err) = &q.error {
            let _ = writeln!(out, "**Error:** {err}\n");
            continue;
        }
        let header: Vec<&str> = q.columns.iter().map(|c| c.name.as_str()).collect();
        let _ = writeln!(out, "| {} |", header.join(" | "));
        let _ = writeln!(out, "|{}", " --- |".repeat(header.len().max(1)));
        for row in &q.rows {
            let cells: Vec<String> = row.iter().map(fmt_query_value).collect();
            let _ = writeln!(out, "| {} |", cells.join(" | "));
        }
        let _ = writeln!(
            out,
            "\n_{} row(s){}_\n",
            q.row_count,
            if q.truncated { ", truncated" } else { "" }
        );
        if let Some(note) = &q.note {
            let _ = writeln!(out, "_Note: {note}_\n");
        }
        render_query_chart(q, out);
    }
}

/// Render an ASCII bar chart beneath the table when the query declared a
/// chartable `-- @viz` directive. `histogram`/`piechart` draw horizontal bars
/// (piecharts also show each slice's share of the total); `treemap` has no
/// ASCII analogue, so a note explains it renders in the HTML report only.
/// `table` and a missing spec draw nothing (the table above suffices). The
/// spec's `cap` limits the number of charted rows (the table always shows all).
fn render_query_chart(q: &crate::query::model::QueryResult, out: &mut String) {
    use crate::query::viz::{VizKind, cell_as_f64, cell_as_label, resolve_columns};
    use std::fmt::Write;

    let Some(spec) = &q.viz else { return };
    if spec.kind == VizKind::Table {
        return;
    }
    // A `title="..."` renders as a heading above the chart (or the treemap note).
    if let Some(title) = &spec.title {
        let _ = writeln!(out, "**{title}**\n");
    }
    if spec.kind == VizKind::Treemap {
        let _ = writeln!(
            out,
            "_Treemap chart is available in the HTML report; showing the table above._\n"
        );
        return;
    }
    // resolve_columns already validated at intake time, but re-resolve so the
    // renderer is self-contained (and degrades to nothing if columns changed).
    let Ok((label_idx, value_idx)) = resolve_columns(spec, &q.columns, &q.rows) else {
        return;
    };

    // Collect (label, value) pairs, skipping rows whose value cell is non-numeric.
    let mut pairs: Vec<(String, f64)> = Vec::new();
    for row in &q.rows {
        if let (Some(lbl), Some(val)) = (
            row.get(label_idx).map(cell_as_label),
            row.get(value_idx).and_then(cell_as_f64),
        ) {
            pairs.push((lbl, val));
        }
    }
    if pairs.is_empty() {
        return;
    }
    if let Some(cap) = spec.cap {
        pairs.truncate(cap);
    }

    let total: f64 = pairs.iter().map(|(_, v)| *v).sum();
    let max = pairs.iter().map(|(_, v)| *v).fold(0.0_f64, f64::max);
    let label_w = pairs
        .iter()
        .map(|(l, _)| l.len())
        .max()
        .unwrap_or(0)
        .min(40);

    let _ = writeln!(out, "```");
    for (label, value) in &pairs {
        let bar = ascii_bar(*value, max, 40);
        let lbl = if label.len() > label_w {
            format!("{}…", &label[..label_w.saturating_sub(1)])
        } else {
            format!("{label:label_w$}")
        };
        if spec.kind == VizKind::Piechart && total > 0.0 {
            let pct = value / total * 100.0;
            let _ = writeln!(out, "{lbl} | {bar} {value:.0} ({pct:.1}%)");
        } else {
            let _ = writeln!(out, "{lbl} | {bar} {value:.0}");
        }
    }
    let _ = writeln!(out, "```\n");
}

/// A proportional ASCII bar of `#` characters: `width` columns at `value/max`.
/// A zero or negative `max` yields an empty bar (avoids div-by-zero / NaN).
fn ascii_bar(value: f64, max: f64, width: usize) -> String {
    if max <= 0.0 || value <= 0.0 {
        return String::new();
    }
    let filled = ((value / max) * width as f64).round() as usize;
    "#".repeat(filled.min(width))
}

/// Format a single `QueryValue` cell for a Markdown table. Pipes inside string
/// values are escaped so they don't break the table; object references render
/// as `class@index`.
fn fmt_query_value(v: &crate::query::model::QueryValue) -> String {
    use crate::query::model::QueryValue as V;
    match v {
        V::Null => "null".into(),
        V::Bool(b) => b.to_string(),
        V::Int(i) => i.to_string(),
        V::Float(f) => format!("{f}"),
        V::Str(s) => s.replace('|', "\\|"),
        V::ObjRef { index, class, .. } => format!("{class}@{index}"),
    }
}

/// Linked in-document table of contents (top-level sections only). Anchors use
/// GitHub's slug convention (lowercase, spaces → hyphens) matching the `##`
/// headings emitted by the section renderers. Kept in lock-step with
/// `render_toc_graphs` so both formats list the same sections.
fn render_toc(r: &Report, out: &mut String) {
    out.push_str("## Contents\n\n");
    out.push_str(&SectionId::Summary.toc_bullet());
    out.push_str(&SectionId::MemoryTriage.toc_bullet());
    if waste_summary_present(r) {
        out.push_str(&SectionId::WasteSummary.toc_bullet());
    }
    out.push_str(&SectionId::SystemOverview.toc_bullet());
    out.push_str(&SectionId::LeakSuspects.toc_bullet());
    out.push_str(&SectionId::TopConsumers.toc_bullet());
    out.push_str(&SectionId::DominatorAnalysis.toc_bullet());
    out.push_str(&SectionId::Threads.toc_bullet());
    if !r.top_components.components.is_empty() {
        out.push_str(&SectionId::TopComponents.toc_bullet());
    }
    out.push_str(&SectionId::ArraysBySize.toc_bullet());
    out.push_str(&SectionId::Collections.toc_bullet());
    if r.collection_attribution.is_some() {
        out.push_str(&SectionId::ContainerAttribution.toc_bullet());
    }
    if r.fields_by_size
        .as_ref()
        .is_some_and(|f| !f.rows.is_empty())
    {
        out.push_str(&SectionId::FieldsBySize.toc_bullet());
    }
    if r.biggest_collections
        .as_ref()
        .is_some_and(|b| !b.combined.is_empty() || !b.by_kind.is_empty())
    {
        out.push_str(&SectionId::BiggestCollections.toc_bullet());
    }
    if r.collection_contents
        .as_ref()
        .is_some_and(|c| !c.rows.is_empty())
    {
        out.push_str(&SectionId::CollectionContents.toc_bullet());
    }
    out.push_str(&SectionId::References.toc_bullet());
    out.push_str(&SectionId::UnreachableObjects.toc_bullet());
    if r.alloc_sites.is_some() {
        out.push_str(&SectionId::AllocationSites.toc_bullet());
    }
    if retention_concentration_present(&r.overview) {
        out.push_str(&SectionId::RetentionConcentration.toc_bullet());
    }
    if depth_stats(&r.overview.dominator_depth_histogram).is_some() {
        out.push_str(&SectionId::DominatorDepth.toc_bullet());
    }
    out.push_str(&SectionId::Glossary.toc_bullet());
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
    out.push_str(SIZE_BASIS_CAPTION);
    out.push_str("\n\n");
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
    stats.row([HEAP_SCALAR_LABEL.into(), format_bytes(o.total_shallow)]);
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
    let pct_of = |retained: u64| -> f64 { pct_of_heap(retained, total) };

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
                fmt_pct(pct_of(s.retained)),
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
                fmt_pct(pct_of(ob.retained)),
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
            "**Likely problem:** `{}` retains {} of the reachable heap — investigate this first.",
            s.pretty_class,
            fmt_pct(pct_of(s.retained)),
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

/// OOM-triage lead-in: a short, human-readable summary of the fired triage
/// signals (evaluated once by the rule framework in `triage.rs` and stored on
/// `Report.triage`). This renderer is a dumb formatter over that list.
pub(crate) fn render_oom_triage(r: &Report, out: &mut String) {
    out.push_str("## Memory Triage\n\n");
    out.push_str("_Automated signals pointing to where memory concentrates and what to investigate first._\n\n");
    for s in &r.triage {
        out.push_str(&format_signal_md(s));
    }
    out.push('\n');
}

/// Format one triage signal as a Markdown bullet. `detail` may contain backtick
/// code spans, which Markdown renders verbatim, so it is emitted as-is.
fn format_signal_md(s: &crate::report::TriageSignal) -> String {
    let link = match (&s.anchor, &s.anchor_label) {
        (Some(anchor), Some(label)) => format!(" See [{label}](#{anchor})."),
        _ => String::new(),
    };
    format!("- **{}:** {}{}\n", s.title, s.detail, link)
}

/// Whether the report has a nonzero Waste Summary to render.
pub(crate) fn waste_summary_present(r: &Report) -> bool {
    r.waste_summary.as_ref().is_some_and(|w| w.total_bytes > 0)
}

/// Waste Summary (§24): one headline "reclaimable N" figure folding every
/// quantifiable waste source (under-filled collections & object arrays,
/// duplicate Strings, String backing-array slack, duplicate primitive arrays),
/// with a per-source breakdown linking into the section that details each.
/// Sources are approximate and may overlap slightly.
pub(crate) fn render_waste_summary(r: &Report, out: &mut String) {
    let Some(w) = r.waste_summary.as_ref() else {
        return;
    };
    if w.total_bytes == 0 {
        return;
    }
    out.push_str("## Waste Summary\n\n");
    out.push_str(&format!(
        "_Approximately **{}** estimated reclaimable across the sources below — \
duplicate strings, duplicate primitive arrays, boxed primitives, and empty/singleton \
collection overhead. Fix the biggest category first for the highest impact. Figures are \
approximate; sources may overlap._\n\n",
        format_bytes(w.total_bytes)
    ));
    let mut t = crate::md::Table::new(
        &["Source", "Reclaimable"],
        &[crate::md::Align::Left, crate::md::Align::Right],
    );
    for s in &w.sources {
        let label = match &s.anchor {
            Some(a) => format!("[{}](#{})", s.label, a),
            None => s.label.clone(),
        };
        t.row([label, format_bytes(s.bytes)]);
    }
    t.render(out);
    out.push('\n');
}

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
         concentration curve: if **Top 1** is already high, one object is the accumulation \
         point — making it unreachable reclaims most of the heap; if the share only climbs as you widen to \
         **Top 10** / **Top 100**, retention is spread across many peers (e.g. a big cache \
         or collection of similar objects) and no single fix helps much._\n\n",
    );
    let mut t = Table::new(
        &["Scope", "Retained Share", "Retained"],
        &[Align::Left, Align::Right, Align::Right],
    );
    t.row([
        "Top 1 object".into(),
        fmt_pct(rc.top1_bp as f64 / 100.0),
        format_bytes(rc.top1_retained),
    ]);
    t.row([
        "Top 10 objects".into(),
        fmt_pct(rc.top10_bp as f64 / 100.0),
        format_bytes(rc.top10_retained),
    ]);
    t.row([
        "Top 100 objects".into(),
        fmt_pct(rc.top100_bp as f64 / 100.0),
        format_bytes(rc.top100_retained),
    ]);
    t.render(out);
    if rc.num_objects_ge_1pct > 0 {
        out.push_str(&format!(
            "\n_{} {} each hold ≥1% of the reachable heap._\n",
            fmt_count(rc.num_objects_ge_1pct),
            plural_objects(rc.num_objects_ge_1pct),
        ));
    }
    out.push('\n');
}

/// Dominator-Depth Distribution (B2): objects per idom-hop below a GC root.
/// Rendered as a standalone section near the end of the report.
pub(crate) fn render_dominator_depth(o: &SystemOverview, out: &mut String) {
    render_dominator_depth_inner(o, false, out);
}

fn render_dominator_depth_inner(o: &SystemOverview, graphs: bool, out: &mut String) {
    use crate::md::{Align, Table, bar};
    let Some(stats) = depth_stats(&o.dominator_depth_histogram) else {
        return;
    };
    // Show up to 50 rows but stop at the last row with >= 0.1% of objects.
    const DEPTH_CAP: usize = 50;
    let meaningful_end = stats
        .rows
        .iter()
        .rposition(|&(_, _, pct, _)| pct >= 0.1)
        .map(|i| i + 1)
        .unwrap_or(stats.rows.len());
    let shown = meaningful_end.min(DEPTH_CAP);

    // Detect a constant-count tail run (e.g. a single linked-list chain where
    // every depth has the same object count). If the last visible row starts a
    // run of identical counts, collapse that run and annotate it as a chain.
    let display_rows = &stats.rows[..shown];
    let chain_start = if display_rows.len() >= 3 {
        let tail_count = display_rows.last().map(|&(_, o, _, _)| o).unwrap_or(0);
        let run_start = display_rows
            .iter()
            .rposition(|&(_, o, _, _)| o != tail_count)
            .map(|i| i + 1)
            .unwrap_or(0);
        // Only collapse runs of 3+ identical-count depths.
        if display_rows.len().saturating_sub(run_start) >= 3 {
            Some(run_start)
        } else {
            None
        }
    } else {
        None
    };
    let visible_end = chain_start.unwrap_or(shown);

    let hidden = stats.rows.len() - shown;
    out.push_str("## Dominator-Depth Distribution\n\n");
    out.push_str(DEPTH_DIST_CAPTION);
    out.push_str(&depth_summary_line(&stats));
    let obj_max = stats.rows[..visible_end.max(1)]
        .iter()
        .map(|&(_, o, _, _)| o)
        .max()
        .unwrap_or(0);
    let mut headers: Vec<&str> = vec!["Depth", "Objects", "% Objects", "Cumulative %"];
    let mut aligns = vec![Align::Right, Align::Right, Align::Right, Align::Right];
    if graphs {
        headers.push("");
        aligns.push(Align::Left);
    }
    let mut t = Table::new(&headers, &aligns);
    for &(depth, objects, pct, cum) in display_rows.iter().take(visible_end) {
        let mut row = vec![
            depth.to_string(),
            fmt_count(objects),
            fmt_pct(pct),
            fmt_pct(cum),
        ];
        if graphs {
            row.push(bar(objects, obj_max, render_graphs::GRAPH_BAR_WIDTH));
        }
        t.row(row);
    }
    t.render(out);
    // Annotate the collapsed chain run if present.
    if let Some(start) = chain_start {
        let chain_rows = &display_rows[start..];
        let chain_objs = chain_rows.first().map(|&(_, o, _, _)| o).unwrap_or(0);
        let first_depth = chain_rows.first().map(|&(d, _, _, _)| d).unwrap_or(0);
        let last_depth = chain_rows.last().map(|&(d, _, _, _)| d).unwrap_or(0);
        let chain_len = last_depth - first_depth + 1;
        out.push_str(&format!(
            "\n_… depths {}–{}: {} hop{} each with {} objects (a single growth-path chain; \
full depth data in JSON)_\n",
            first_depth,
            last_depth,
            chain_len,
            if chain_len == 1 { "" } else { "s" },
            fmt_count(chain_objs),
        ));
    }
    if hidden > 0 {
        // Count objects and compute cumulative % for the hidden tail
        let hidden_objects: u64 = stats.rows[shown..].iter().map(|&(_, o, _, _)| o).sum();
        let last_cum = stats.rows.last().map(|&(_, _, _, c)| c).unwrap_or(0.0);
        out.push_str(&format!(
            "\n_… (+{} deeper buckets, {} objects, {} cumulative — full data in JSON)_\n",
            hidden,
            fmt_count(hidden_objects),
            fmt_pct(last_cum),
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
- **Retained heap (retained size)**: the total memory that would be reclaimed if this
  object became unreachable — its own shallow size plus everything
  reachable *only* through it. This is the number that answers \"how much would
  making it unreachable reclaim?\" and it is the basis for every percentage in this
  report. See [dominator (graph theory)](https://en.wikipedia.org/wiki/Dominator_(graph_theory)).
- **Reachable heap**: all objects the [garbage collector](https://en.wikipedia.org/wiki/Garbage_collection_(computer_science)) can still
  reach from a GC root. Anything unreachable is already collectible and is excluded
  from the totals here.
- **GC root**: an object the JVM keeps alive unconditionally, such as live thread
  stacks (local variables), static fields of loaded classes,
  [JNI](https://en.wikipedia.org/wiki/Java_Native_Interface) references, and
  similar. Every retained-size chain ends at a GC root.
- **Dominator**: object *A* dominates object *B* if every path from a GC root to
  *B* passes through *A*. In other words, if *A* became unreachable, *B* would become
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
- **Collection fill ratio**: the fraction of a collection's backing-array capacity
  that is actually occupied by elements — `elements / capacity`. A fill ratio near
  0 means the backing array is mostly empty (wasted memory). A ratio near 1 means
  the collection is full.
- **Map Load Factor**: for hash maps, the fraction of backing-array
  slots occupied — `occupied_slots / capacity`. A low load factor means many
  empty buckets (wasted memory); a very high load factor increases hash collision
  probability and lookup cost.
- **Only-weakly retained**: an object that has no incoming strong reference — it is
  reachable only through one or more `WeakReference`, `SoftReference`, or
  `PhantomReference` chains. Weak-only referents are collected at the next GC cycle;
  soft-only referents are collected under memory pressure; phantom-only referents are
  already unreachable and queued for resource cleanup.
- **Compressed OOPs** (Compressed Ordinary Object Pointers): a JVM optimisation
  where object references are stored as 32-bit integers instead of 64-bit pointers,
  halving reference-field overhead on heaps <= ~32 GB. Visible in the Heap Summary
  as `Compressed OOPs: yes`.
- **Class#field**: the notation used throughout this report to identify a specific
  field — `HolderClass#fieldName`. For example `java.util.HashMap#table` names the
  `table` field of `HashMap`. This is the dominant incoming reference path for an
  object, not a guaranteed allocation site — it is a hint, not a precise origin.
";

/// Render the "System Overview" section (plain Markdown): scalars, GC-roots and
/// heap-composition breakdowns, and the full class histogram. Byte-exact-tested.
pub(crate) fn render_system_overview(o: &SystemOverview, off_heap_cap: u64, out: &mut String) {
    use crate::md::{Align, Table};
    out.push_str("## System Overview\n\n");
    out.push_str("_JVM and dump metadata, heap totals, GC root breakdown, class loader sizes, and system properties._\n\n");
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
    summary.row([HEAP_SCALAR_LABEL.into(), format_bytes(o.total_shallow)]);
    if off_heap_cap > 0 {
        let ratio_str = if o.total_shallow > 0 {
            format!(
                "{} off-heap ({:.1}× on-heap)",
                format_bytes(off_heap_cap),
                off_heap_cap as f64 / o.total_shallow as f64,
            )
        } else {
            format!("{} off-heap", format_bytes(off_heap_cap))
        };
        summary.row(["Off-heap / on-heap".into(), ratio_str]);
    }
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
            "Heap fragmentation (unreachable / heap total)".into(),
            fmt_pct(o.heap_fragmentation_ratio * 100.0),
        ]);
    }
    if o.top_class_concentration_bp > 0 {
        summary.row([
            "Top-class retained concentration".into(),
            fmt_pct(o.top_class_concentration_bp as f64 / 100.0),
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
        use crate::md::bar;
        out.push_str("### GC Roots by Type\n\n");
        out.push_str(
            "_GC roots are the entry points where the JVM starts reachability scanning — \
anything reachable from a root stays alive. Common root types: thread-stack locals, \
JNI global references, static fields of loaded classes, and synchronized lock objects._\n\n",
        );
        let max_count = o
            .gc_roots_by_type
            .iter()
            .map(|r| r.count)
            .max()
            .unwrap_or(0);
        let mut t = Table::new(
            &["Root Type", "Count", ""],
            &[Align::Left, Align::Right, Align::Left],
        );
        for row in &o.gc_roots_by_type {
            t.row([
                row.root_type.clone(),
                fmt_count(row.count),
                bar(row.count, max_count, 16),
            ]);
        }
        t.render(out);
        out.push('\n');
    }

    // Heap composition by kind: worth a table only when >1 kind present
    // (a single-kind heap just restates "Total objects").
    if o.heap_composition.by_kind.len() > 1 {
        use crate::md::bar;
        out.push_str("### Heap Composition\n\n");
        out.push_str(
            "_Shallow heap broken down by object kind — instance objects, object arrays, and primitive arrays._\n\n",
        );
        let max_shallow = o
            .heap_composition
            .by_kind
            .iter()
            .map(|k| k.shallow_heap)
            .max()
            .unwrap_or(0);
        let mut t = Table::new(
            &["Kind", "Objects", "Shallow Heap", ""],
            &[Align::Left, Align::Right, Align::Right, Align::Left],
        );
        for k in &o.heap_composition.by_kind {
            t.row([
                k.kind.clone(),
                fmt_count(k.objects),
                format_bytes(k.shallow_heap),
                bar(k.shallow_heap, max_shallow, 16),
            ]);
        }
        t.render(out);
        out.push('\n');
    }

    render_record_census(out, &o.record_census);
    render_duplicate_strings(out, &o.duplicate_strings, false);
    render_duplicate_prim_arrays(out, &o.duplicate_prim_arrays);
    render_boxed_numbers(
        out,
        &o.boxed_numbers,
        &o.boxed_number_holders,
        o.total_shallow,
    );
    render_header_overhead(out, &o.header_overhead);

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
            "% Heap",
        ],
        &[
            Align::Right,
            Align::Left,
            Align::Right,
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
            fmt_pct(pct_of_heap(row.retained, o.total_shallow)),
        ]);
    }
    hist.render(out);
    if o.histogram.len() > 50 {
        let remaining = o.histogram.len() - 50;
        let tail_shallow: u64 = o.histogram[50..].iter().map(|r| r.shallow).sum();
        let tail_retained: u64 = o.histogram[50..].iter().map(|r| r.retained).sum();
        out.push_str(&format!(
            "_… {} more classes, {} shallow / {} retained (full list in JSON)._\n",
            fmt_count(remaining as u64),
            format_bytes(tail_shallow),
            format_bytes(tail_retained),
        ));
    }
    out.push('\n');

    // Class Loaders (F2): per-loader rollup, top-N by retained heap.
    if !o.loader_rollup.is_empty() {
        out.push_str("### Class Loaders\n\n");
        out.push_str(
            "_Classes grouped by the loader that defined them. \
The **Loader** column shows the loader's class (e.g. `java/net/URLClassLoader`), \
not an instance name — the hprof format does not record loader names. \
Multiple rows with the same loader class are distinct loader instances; \
many such instances each holding significant heap can signal a class-loader leak. \
The **Address** column distinguishes them._\n\n",
        );
        let mut t = Table::new(
            &[
                "Loader",
                "Address",
                "Classes",
                "Instances",
                "Shallow Heap",
                "Retained Heap",
            ],
            &[
                Align::Left,
                Align::Left,
                Align::Right,
                Align::Right,
                Align::Right,
                Align::Right,
            ],
        );
        for r in &o.loader_rollup {
            let addr = if r.loader_id == 0 {
                "<boot>".into()
            } else {
                format!("0x{:x}", r.loader_id)
            };
            t.row([
                r.loader_label.clone().unwrap_or_else(|| "<unknown>".into()),
                addr,
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
            "_Class names loaded by more than one class loader. \
The same class loaded N times means N separate copies of its static state and \
N times the metaspace cost — a typical symptom of class-loader leaks (e.g. \
each web-app reload or plugin load creates a new loader that never gets GC'd). \
Check the per-loader breakdown: if one loader holds almost all the instances \
the others are likely leaked copies._\n\n",
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
pub(crate) fn render_leak_suspects(l: &LeakSuspects, out: &mut String) {
    out.push_str("## Leak Suspects\n\n");

    if l.suspects.is_empty() {
        out.push_str("No single object or class group exceeds the threshold.\n\n");
        return;
    }

    out.push_str(
        "_Objects and class groups retaining the most heap, ranked by retained size. \
These are the most likely accumulation points for excessive memory usage. \
To fix: follow the dominator chain to the nearest object you control \
and drop or null out the reference that keeps it alive. \
The path to GC root is shown for each suspect below._\n\n",
    );

    for (rank, s) in l.suspects.iter().enumerate() {
        let pct = pct_of_heap(s.retained, l.total_shallow);

        out.push_str(&format!(
            "### {}. `{}` — retains {} ({} of {HEAP_BASIS_LABEL})\n\n",
            rank + 1,
            s.pretty_class,
            format_bytes(s.retained),
            fmt_pct(pct),
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
            if s.pretty_class == "java.lang.Class" {
                out.push_str(
                    "_Note: `java.lang.Class` objects are normal — every loaded class has one. \
This suspect reflects class-metadata memory, not a leak in application code. \
It is worth investigating only if the instance count is unexpectedly high \
(e.g. due to class-loader leaks)._\n\n",
                );
            }
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

        // Accumulated objects: by-class histogram only (the per-instance list
        // is redundant for most cases and inflates the report).
        if !s.dominated_by_class.is_empty() {
            use crate::md::{Align, Table};
            if s.dominated_total_count > s.dominated_shown {
                out.push_str(&format!(
                    "_Directly dominates {} {} (showing top {} classes by retained heap)._\n\n",
                    fmt_count(s.dominated_total_count),
                    plural_objects(s.dominated_total_count),
                    fmt_count(s.dominated_by_class.len() as u64),
                ));
            } else if s.dominated_total_count > 0 {
                out.push_str(&format!(
                    "_Directly dominates {} {}._\n\n",
                    fmt_count(s.dominated_total_count),
                    plural_objects(s.dominated_total_count),
                ));
            }
            out.push_str("**Accumulated objects by class:**\n\n");
            let mut t = Table::new(
                &["Class", "Objects", "Shallow", "Retained", "% of suspect"],
                &[
                    Align::Left,
                    Align::Right,
                    Align::Right,
                    Align::Right,
                    Align::Right,
                ],
            );
            for row in &s.dominated_by_class {
                let pct_str = if s.retained > 0 {
                    fmt_pct(pct_of_heap(row.retained, s.retained))
                } else {
                    "—".to_string()
                };
                t.row([
                    format!("`{}`", row.pretty_class),
                    fmt_count(row.instances),
                    format_bytes(row.shallow),
                    format_bytes(row.retained),
                    pct_str,
                ]);
            }
            t.render(out);
            out.push('\n');
        }

        // Dominator chain to a GC root (single suspects only).
        if let Some(path) = &s.root_path {
            render_root_path(path, out);
        }
        // Full multi-level dominator subtree at the accumulation point — in a
        // collapsible block to keep the report readable.
        if let Some(tree) = &s.dominator_tree {
            out.push_str("<details>\n<summary>Dominator subtree</summary>\n\n");
            render_dom_tree_plain(tree, out);
            out.push_str("</details>\n\n");
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
pub(crate) fn render_top_consumers(t: &TopConsumers, total_shallow: u64, out: &mut String) {
    use crate::md::{Align, Table};
    out.push_str("## Top Consumers\n\n");
    out.push_str("### Biggest Objects (Top-Level Dominators)\n\n");
    out.push_str(
        "_All top-level dominators ranked by retained heap. Unlike Leak Suspects, \
this list is unfiltered — it includes every object directly dominated by a GC root, \
down to the smallest. Use it when the suspect you care about didn't cross the \
leak-suspect threshold, or to see the full retention picture._\n\n",
    );
    // The "Held via" column names the dominant incoming `Class#field` reference
    // (the primary referrer; an object may have others). Present only when
    // attribution data exists (i.e. `--collections` was passed).
    let obj_has_owner = t.biggest_objects.iter().any(|r| r.owner.is_some());
    if obj_has_owner {
        out.push_str(
            "_The **Held via** column names the dominant incoming `Class#field` reference \
that holds each object (the primary referrer; an object may have several)._\n\n",
        );
    }
    let mut obj_headers: Vec<&str> = vec!["#", "Class", "Shallow", "Retained", "% Heap"];
    let mut obj_aligns = vec![
        Align::Right,
        Align::Left,
        Align::Right,
        Align::Right,
        Align::Right,
    ];
    if obj_has_owner {
        obj_headers.push("Held via (Class#field)");
        obj_aligns.push(Align::Left);
    }
    let mut objs = Table::new(&obj_headers, &obj_aligns);
    for (rank, row) in t.biggest_objects.iter().enumerate() {
        let pct = pct_of_heap(row.retained, total_shallow);
        let mut cells = vec![
            (rank + 1).to_string(),
            format!("`{}`", row.display_class),
            format_bytes(row.shallow),
            format_bytes(row.retained),
            fmt_pct(pct),
        ];
        if obj_has_owner {
            cells.push(match &row.owner {
                Some(o) => format!("`{o}`"),
                None => "—".to_string(),
            });
        }
        objs.row(cells);
    }
    objs.render(out);
    out.push('\n');

    out.push_str("### Biggest Classes by Retained Heap\n\n");
    out.push_str("_Classes ranked by total retained heap. High retained with low shallow means the class is keeping many other objects alive — investigate it in Dominator Analysis._\n\n");
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
            "_Retained heap distributed across all {} top-level dominators. The shape reveals whether \
a handful of large objects dominate the heap or memory is scattered across many small ones._\n\n",
            fmt_count(d.count)
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
        let mut buckets = Table::new(
            &["Size ≤", "Count", "% of Dom."],
            &[Align::Right, Align::Right, Align::Right],
        );
        for b in &d.buckets {
            let pct = if d.count > 0 {
                fmt_pct(b.count as f64 / d.count as f64 * 100.0)
            } else {
                "—".into()
            };
            buckets.row([format_bytes(b.upper_bytes), fmt_count(b.count), pct]);
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
        "_Per-thread retained heap and properties. A thread keeps everything on its \
stack alive — blocked or long-running threads can hold significant memory through \
local variables._\n\n",
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
            .map(|s| escape_string_cell(&s))
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
                th.thread_serial,
                escape_string_cell(name),
                class
            )),
            _ => out.push_str(&format!("### Thread {} ({})\n\n", th.thread_serial, class)),
        }
        out.push_str(&format!(
            "_Local roots: {}._\n\n",
            fmt_count(th.local_root_count)
        ));
        // A bounded table of this thread's local root objects (empty for
        // threads with no resolved locals ⇒ nothing emitted).
        if let Some(objs) = &th.local_objects {
            if !objs.is_empty() && objs.len() < th.local_root_count as usize {
                out.push_str(&format!(
                    "_Showing top {} by retained heap (sizes overlap and do not sum to thread total)._\n\n",
                    fmt_count(objs.len() as u64),
                ));
            }
            render_thread_locals(objs, out);
        }
        // Significant-frames interleave (frames with their retained locals),
        // when locals were sampled; otherwise the plain frame list.
        if !th.significant_frames.is_empty() {
            out.push_str(&format!(
                "_Frame percentages are of this thread's {} retained heap._\n\n",
                format_bytes(th.retained)
            ));
            for sf in &th.significant_frames {
                out.push_str(&format!("- `{}`\n", sf.frame));
                for loc in &sf.locals {
                    out.push_str(&format!(
                        "  - `{}` retains {} ({} of thread retained)\n",
                        loc.display_class,
                        format_bytes(loc.retained),
                        fmt_pct(loc.pct)
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
        &["Object", "Count", "Shallow", "Retained"],
        &[Align::Left, Align::Right, Align::Right, Align::Right],
    );
    // Collapse identical (class, shallow, retained) rows into ×N.
    let mut i = 0;
    while i < objs.len() {
        let o = &objs[i];
        let count = objs[i..]
            .iter()
            .take_while(|x| {
                x.display_class == o.display_class
                    && x.shallow == o.shallow
                    && x.retained == o.retained
            })
            .count();
        let count_str = if count > 1 {
            format!("×{}", fmt_count(count as u64))
        } else {
            "1".into()
        };
        t.row([
            format!("`{}`", o.display_class),
            count_str,
            format_bytes(o.shallow),
            format_bytes(o.retained),
        ]);
        i += count;
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
        "_Retained heap grouped by class loader (component). `% Heap` is the share of total reachable heap. \
Totals can exceed heap size because boot-loader classes are counted in every component that retains them._\n\n",
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
            fmt_pct(c.pct),
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
    use crate::md::{Align, Table, bar};
    out.push_str("## Arrays by Size\n\n");
    if a.obj_array_buckets.is_empty() && a.prim_array_buckets.is_empty() && a.zero_length_count == 0
    {
        out.push_str("_No arrays found._\n\n");
        return;
    }
    out.push_str(
        "_Array-length distribution bucketed by power-of-two element length. \
Helps spot unexpectedly large arrays or many tiny zero-length allocations. \
`Max length` is the inclusive upper bound of each bucket._\n\n",
    );

    let render_table = |title: &str, buckets: &[SizeHistogramBucket], out: &mut String| {
        out.push_str(&format!("### {title}\n\n"));
        if buckets.is_empty() {
            out.push_str("_No data for this section._\n\n");
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
        let total_objects: u64 = buckets.iter().map(|b| b.objects).sum();
        let total_shallow: u64 = buckets.iter().map(|b| b.shallow).sum();
        let mut total_row = vec![
            "**Total**".to_string(),
            format!("**{}**", fmt_count(total_objects)),
            format!("**{}**", format_bytes(total_shallow)),
        ];
        if graphs {
            total_row.push(String::new());
        }
        t.row(total_row);
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
    if b.lower_ratio_bp == b.upper_ratio_bp {
        format!("{lo:.0}% (full)")
    } else {
        format!("{lo:.0}–{hi:.0}%")
    }
}

/// Render the always-on "Collections" section: five collection/array sub-views
/// (fill ratio, size histogram, object-array fill ratio, map collision ratio,
/// and constant primitive arrays). Shared by plain md and md-graphs; when
/// `graphs` is set, an extra proportional bar column is appended on the object
/// count of each table. Emits the heading + fallback italic lines even when
/// empty so the document structure stays stable.
/// Render a fill-ratio bucket table (`Collection Fill Ratio`, `Array Fill
/// Ratio`, `Map Load Factor`). `count_header` names the object column
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
    use crate::md::{Align, Table, bar};
    if buckets.is_empty() {
        out.push_str("_No data for this section._\n\n");
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
    let total_objects: u64 = buckets.iter().map(|b| b.objects).sum();
    let total_shallow: u64 = buckets.iter().map(|b| b.shallow).sum();
    let mut total_row = vec![
        "**Total**".to_string(),
        format!("**{}**", fmt_count(total_objects)),
        format!("**{}**", format_bytes(total_shallow)),
    ];
    if with_wasted {
        let total_wasted: u64 = buckets.iter().map(|b| b.wasted).sum();
        total_row.push(format!("**{}**", format_bytes(total_wasted)));
    }
    if graphs {
        total_row.push(String::new());
    }
    t.row(total_row);
    t.render(out);
    out.push('\n');
}

/// Render a compact "Likely wasters" list from attribution, filtered to rows
/// whose `container_kind` matches one of `kinds`, sorted by `total_wasted_bytes`
/// descending. Shows at most `n` entries. Skipped when attribution is absent or
/// no rows have any wasted slots.
fn render_top_contributors(
    attribution: &Option<CollectionAttribution>,
    kinds: &[&str],
    n: usize,
    out: &mut String,
) {
    use crate::md::{Align, Table};
    let Some(a) = attribution else { return };
    let mut rows: Vec<_> = a
        .most_overall
        .iter()
        .filter(|r| kinds.iter().any(|k| *k == r.container_kind) && r.total_wasted_slots > 0)
        .collect();
    rows.sort_by_key(|r| std::cmp::Reverse(r.total_wasted_bytes.max(r.total_wasted_slots)));
    rows.truncate(n);
    if rows.is_empty() {
        return;
    }
    out.push_str("_Likely wasters by field (dominant incoming `Class#field` referrer):_\n\n");
    let has_bytes = rows.iter().any(|r| r.total_wasted_bytes > 0);
    let waste_header = if has_bytes {
        "Wasted Bytes"
    } else {
        "Wasted Slots"
    };
    let mut t = Table::new(
        &[
            "Class#field",
            "Containers",
            waste_header,
            "Total Elements",
            "Total Retained",
        ],
        &[
            Align::Left,
            Align::Right,
            Align::Right,
            Align::Right,
            Align::Right,
        ],
    );
    for r in &rows {
        let waste_cell = if has_bytes {
            format_bytes(r.total_wasted_bytes)
        } else {
            fmt_count(r.total_wasted_slots)
        };
        t.row(vec![
            format!("`{}#{}`", r.holder_class, r.field),
            fmt_count(r.container_count),
            waste_cell,
            fmt_count(r.total_elements),
            format_bytes(r.total_retained),
        ]);
    }
    t.render(out);
    out.push('\n');
}

/// Render the single most-wasted container per field, filtered by kind, sorted
/// by wasted slots (capacity - elements) descending. Skipped when absent.
fn render_worst_single_containers(
    attribution: &Option<CollectionAttribution>,
    kinds: &[&str],
    n: usize,
    out: &mut String,
) {
    use crate::md::{Align, Table};
    let Some(a) = attribution else { return };
    let mut rows: Vec<_> = a
        .biggest_single
        .iter()
        .filter(|r| kinds.iter().any(|k| *k == r.container_kind) && r.capacity > r.elements)
        .collect();
    rows.sort_by(|a, b| {
        b.capacity
            .saturating_sub(b.elements)
            .cmp(&a.capacity.saturating_sub(a.elements))
    });
    rows.truncate(n);
    if rows.is_empty() {
        return;
    }
    out.push_str("_Worst individual containers (most empty slots):_\n\n");
    let mut t = Table::new(
        &[
            "Class#field",
            "Container Class",
            "Used",
            "Capacity",
            "Wasted Slots",
            "Retained",
        ],
        &[
            Align::Left,
            Align::Left,
            Align::Right,
            Align::Right,
            Align::Right,
            Align::Right,
        ],
    );
    for r in &rows {
        let wasted = r.capacity.saturating_sub(r.elements);
        t.row(vec![
            format!("`{}#{}`", r.holder_class, r.field),
            format!("`{}`", r.container_class),
            fmt_count(r.elements),
            fmt_count(r.capacity),
            fmt_count(wasted),
            format_bytes(r.retained),
        ]);
    }
    t.render(out);
    out.push('\n');
}

pub(crate) fn render_collections(
    c: &CollectionsAnalysis,
    attribution: &Option<CollectionAttribution>,
    graphs: bool,
    out: &mut String,
) {
    use crate::md::{Align, Table, bar};
    out.push_str("## Collections\n\n");
    out.push_str(
        "_Collection fill ratios, map load factors, and constant-value primitive array groups. \
Low fill ratios waste backing-array memory; high load factors increase hash-bucket \
collisions and degrade lookup performance._\n\n",
    );

    // ── Collections by Kind ──────────────────────────────────────────────────
    out.push_str("### Collections by Kind\n\n");
    if c.kind_summary.kinds.is_empty() {
        out.push_str("_No collection kinds found in this heap._\n\n");
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
        let total_count: u64 = c.kind_summary.kinds.iter().map(|s| s.count).sum();
        let total_elements: u64 = c.kind_summary.kinds.iter().map(|s| s.total_elements).sum();
        let total_shallow: u64 = c.kind_summary.kinds.iter().map(|s| s.total_shallow).sum();
        let mut total_row = vec![
            "**Total**".to_string(),
            format!("**{}**", fmt_count(total_count)),
            format!("**{}**", fmt_count(total_elements)),
            String::new(),
            format!("**{}**", format_bytes(total_shallow)),
        ];
        if graphs {
            total_row.push(String::new());
        }
        t.row(total_row);
        t.render(out);
        out.push('\n');
    }
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
    render_top_contributors(
        attribution,
        &["list", "set", "deque", "queue", "tree", "mixed"],
        10,
        out,
    );
    render_worst_single_containers(
        attribution,
        &["list", "set", "deque", "queue", "tree", "mixed"],
        5,
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
        out.push_str("_No size distribution data — all tracked collections may be empty._\n\n");
    } else {
        let obj_max = c
            .collections_by_size
            .buckets
            .iter()
            .map(|b| b.objects)
            .max()
            .unwrap_or(0);
        let mut headers: Vec<&str> = vec!["Size ≤", "Collections", "Total Shallow"];
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
        let total_objects: u64 = c
            .collections_by_size
            .buckets
            .iter()
            .map(|b| b.objects)
            .sum();
        let total_shallow: u64 = c
            .collections_by_size
            .buckets
            .iter()
            .map(|b| b.shallow)
            .sum();
        let mut total_row = vec![
            "**Total**".to_string(),
            format!("**{}**", fmt_count(total_objects)),
            format!("**{}**", format_bytes(total_shallow)),
        ];
        if graphs {
            total_row.push(String::new());
        }
        t.row(total_row);
        t.render(out);
        out.push('\n');
    }
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
    render_top_contributors(attribution, &["object array"], 10, out);
    render_worst_single_containers(attribution, &["object array"], 5, out);

    // ── Map Load Factor ──────────────────────────────────────────────────────
    out.push_str("### Map Load Factor\n\n");
    out.push_str(&format!(
        "_{} tracked of {} maps (occupied slots ÷ capacity; high values ≥ 90% increase collision chains)._\n\n",
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
    render_top_contributors(attribution, &["map"], 10, out);
    render_worst_single_containers(attribution, &["map"], 5, out);

    // ── Constant Primitive Arrays ────────────────────────────────────────────
    out.push_str("### Constant Primitive Arrays\n\n");
    // Filter noise: skip groups that are trivially short (length <= 4) and have
    // few instances — these are almost always single-char String backing arrays
    // (e.g. byte[1] value=49) with no actionable information.
    const MIN_LENGTH: u64 = 8;
    const MIN_INSTANCES: u64 = 5;
    let interesting_rows: Vec<_> = c
        .constant_primitive_arrays
        .rows
        .iter()
        .filter(|r| r.length >= MIN_LENGTH || r.objects >= MIN_INSTANCES)
        .collect();
    let mut note = String::from(
        "_Primitive arrays whose every element is identical — possible candidates for \
deduplication or replacement with a shared constant. Short arrays (length < 8 with \
few instances) are hidden as noise._",
    );
    if c.constant_primitive_arrays.truncated {
        note.push_str(" _(list truncated; remaining groups folded into one row)._");
    }
    out.push_str(&note);
    out.push_str("\n\n");
    let skipped = c.constant_primitive_arrays.rows.len() - interesting_rows.len();
    if skipped > 0 {
        out.push_str(&format!("_({skipped} trivial groups hidden.)_\n\n"));
    }
    if interesting_rows.is_empty() {
        out.push_str("_No constant primitive arrays found._\n\n");
    } else {
        let obj_max = interesting_rows
            .iter()
            .map(|r| r.objects)
            .max()
            .unwrap_or(0);
        // The Owner column (dominant `Class#field` referrer) is present only when
        // attribution data exists (i.e. `--collections` was passed).
        let has_owner = interesting_rows.iter().any(|r| r.owner.is_some());
        let mut headers: Vec<&str> = vec!["Array class", "Length", "Value", "Objects", "Shallow"];
        let mut aligns = vec![
            Align::Left,
            Align::Right,
            Align::Right,
            Align::Right,
            Align::Right,
        ];
        if has_owner {
            headers.push("Owner (Class#field)");
            aligns.push(Align::Left);
        }
        if graphs {
            headers.push("");
            aligns.push(Align::Left);
        }
        let mut t = Table::new(&headers, &aligns);
        for r in &interesting_rows {
            let mut row = vec![
                format!("`{}`", r.array_class),
                fmt_count(r.length),
                format!("{}", r.value),
                fmt_count(r.objects),
                format_bytes(r.shallow),
            ];
            if has_owner {
                row.push(match &r.owner {
                    Some(o) => format!("`{o}`"),
                    None => "—".to_string(),
                });
            }
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
    use crate::md::{Align, Table, bar};

    out.push_str(&format!("### Top Arrays ({kind})\n\n"));
    out.push_str(&format!(
        "_The largest {kind} arrays by shallow size, individually and aggregated by array class._\n\n"
    ));

    // Largest individual arrays.
    if t.top_individual.is_empty() {
        out.push_str("_No data for this section._\n\n");
    } else {
        let sh_max = t
            .top_individual
            .iter()
            .map(|r| r.shallow)
            .max()
            .unwrap_or(0);
        // "Used/Length" fill column is shown when any row carries non_null data
        // (object arrays only; primitive arrays always have non_null = None).
        let has_fill = t.top_individual.iter().any(|r| r.non_null.is_some());
        // The Owner column is present when any row has an owner label.
        let has_owner = t.top_individual.iter().any(|r| r.owner.is_some());
        let mut headers: Vec<&str> = vec!["Array class", "Length"];
        let mut aligns = vec![Align::Left, Align::Right];
        if has_fill {
            headers.push("Used/Length");
            aligns.push(Align::Right);
        }
        headers.push("Shallow");
        aligns.push(Align::Right);
        if has_owner {
            headers.push("Owner (Class#field)");
            aligns.push(Align::Left);
        }
        if graphs {
            headers.push("");
            aligns.push(Align::Left);
        }
        let mut tbl = Table::new(&headers, &aligns);
        for r in &t.top_individual {
            let mut row = vec![format!("`{}`", r.array_class), fmt_count(r.length)];
            if has_fill {
                row.push(match r.non_null {
                    Some(nn) => format!("{}/{}", fmt_count(nn), fmt_count(r.length)),
                    None => "—".to_string(),
                });
            }
            row.push(format_bytes(r.shallow));
            if has_owner {
                row.push(match &r.owner {
                    Some(o) => format!("`{o}`"),
                    None => "—".to_string(),
                });
            }
            if graphs {
                row.push(bar(r.shallow, sh_max, render_graphs::GRAPH_BAR_WIDTH));
            }
            tbl.row(row);
        }
        let total_shallow: u64 = t.top_individual.iter().map(|r| r.shallow).sum();
        let mut total_row = vec!["**Total**".to_string(), String::new()];
        if has_fill {
            total_row.push(String::new());
        }
        total_row.push(format!("**{}**", format_bytes(total_shallow)));
        if has_owner {
            total_row.push(String::new());
        }
        if graphs {
            total_row.push(String::new());
        }
        tbl.row(total_row);
        tbl.render(out);
        out.push('\n');
    }

    // Largest array classes by aggregate shallow.
    out.push_str(&format!("#### Top Array Classes ({kind})\n\n"));
    if t.top_by_class.is_empty() {
        out.push_str("_No data for this section._\n\n");
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
        let total_instances: u64 = t.top_by_class.iter().map(|r| r.objects).sum();
        let total_shallow: u64 = t.top_by_class.iter().map(|r| r.shallow).sum();
        let mut total_row = vec![
            "**Total**".to_string(),
            format!("**{}**", fmt_count(total_instances)),
            format!("**{}**", format_bytes(total_shallow)),
        ];
        if graphs {
            total_row.push(String::new());
        }
        tbl.row(total_row);
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
    use crate::md::{Align, Table, bar};
    let Some(a) = a else {
        return;
    };

    out.push_str("## Container Attribution\n\n");
    out.push_str(
        "_Which holder `Class#field` points at the most container memory. Two rankings: total \
         across all containers reached through a field, and the single largest container per \
         field. To reduce waste: shrink the collection's initial capacity, evict unused entries, \
         or null out the field when the holder is done._\n\n",
    );

    // ── Most Overall ─────────────────────────────────────────────────────────
    out.push_str("### Top by Total Memory\n\n");
    if a.most_overall.is_empty() {
        out.push_str("_No collections exceeded the size threshold._\n\n");
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
    out.push_str("### Largest Single Container\n\n");
    if a.biggest_single.is_empty() {
        out.push_str("_No single-element collections found._\n\n");
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
            "Kind",
            "Elements",
            "Capacity",
            "Retained",
        ];
        let mut aligns = vec![
            Align::Left,
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
                r.container_kind.clone(),
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

    // ── Tiny Collection Overhead ─────────────────────────────────────────────
    out.push_str("### Tiny Collection Overhead\n\n");
    out.push_str(
        "_Empty (size-0) and singleton (size-1) collections whose wrapper objects are unnecessary. \
         Replace with `null` or a direct field reference; wrapper overhead per collection is \
         one object header plus backing-array pointer._\n\n",
    );
    if a.tiny_overhead.is_empty() {
        out.push_str("_None._\n\n");
    } else {
        let oh_max = a
            .tiny_overhead
            .iter()
            .map(|r| r.overhead_bytes)
            .max()
            .unwrap_or(0);
        let mut headers: Vec<&str> = vec!["Class#field", "Kind", "Empty", "Size-1", "Overhead"];
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
        for r in &a.tiny_overhead {
            let mut row = vec![
                format!("`{}#{}`", r.holder_class, r.field),
                r.container_kind.clone(),
                fmt_count(r.empty_count),
                fmt_count(r.singleton_count),
                format_bytes(r.overhead_bytes),
            ];
            if graphs {
                row.push(bar(
                    r.overhead_bytes,
                    oh_max,
                    render_graphs::GRAPH_BAR_WIDTH,
                ));
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

/// `Class#field` holders ranked by total retained size of everything the field
/// points at. Present only when `--collections` was passed. Shared by plain md
/// and md-graphs; when `graphs` is set a proportional bar is appended on the
/// Retained column.
pub(crate) fn render_fields_by_size(f: &Option<FieldsBySize>, graphs: bool, out: &mut String) {
    use crate::md::{Align, Table, bar};
    let Some(f) = f else {
        return;
    };

    out.push_str("## Fields by Retained Size\n\n");
    out.push_str(
        "_Which holder `Class#field` retains the most memory, summed over every object the \
         field points at. Runtime pointee type is the dominant concrete class reached through \
         the field (`varies` when no single type dominates). A field retaining unexpectedly \
         large memory is a good candidate to null after use or replace with a lazy-initialized \
         reference._\n\n",
    );

    if f.rows.is_empty() {
        out.push_str(
            "_No field-size data — pass `--collections` to enable field attribution._\n\n",
        );
        return;
    }

    if f.truncated {
        out.push_str(
            "_Field grouping was truncated (group or pointee cap hit); ranking is a bounded \
             sample._\n\n",
        );
    }
    let ret_max = f.rows.iter().map(|r| r.total_retained).max().unwrap_or(0);
    // Only show Elements when at least one row is a collection (elements > 0).
    let has_elements = f.rows.iter().any(|r| r.elements > 0);
    let mut headers: Vec<&str> = vec![
        "Class#field",
        "Runtime Pointee Type",
        "Category",
        "Pointees",
    ];
    let mut aligns = vec![Align::Left, Align::Left, Align::Left, Align::Right];
    if has_elements {
        headers.push("Elements");
        aligns.push(Align::Right);
    }
    headers.extend_from_slice(&["Holder Instances", "Sharing", "Retained"]);
    aligns.extend_from_slice(&[Align::Right, Align::Right, Align::Right]);
    if graphs {
        headers.push("");
        aligns.push(Align::Left);
    }
    let mut t = Table::new(&headers, &aligns);
    let mut total_retained = 0u64;
    let mut total_pointees = 0u64;
    let mut total_elements = 0u64;
    for r in &f.rows {
        total_retained += r.total_retained;
        total_pointees += r.pointees;
        total_elements += r.elements;
        let mut row = vec![
            format!("`{}#{}`", r.holder_class, r.field),
            format!("`{}`", r.pointee_type),
            r.category.clone(),
            fmt_count(r.pointees),
        ];
        if has_elements {
            row.push(fmt_count(r.elements));
        }
        row.push(fmt_count(r.holder_instances));
        let sharing = if r.holder_instances > 0 {
            format!("{:.1}×", r.pointees as f64 / r.holder_instances as f64)
        } else {
            "—".into()
        };
        row.push(sharing);
        row.push(format_bytes(r.total_retained));
        if graphs {
            row.push(bar(
                r.total_retained,
                ret_max,
                render_graphs::GRAPH_BAR_WIDTH,
            ));
        }
        t.row(row);
    }
    let mut total_row = vec![
        "**Total**".to_string(),
        String::new(),
        String::new(),
        fmt_count(total_pointees),
    ];
    if has_elements {
        total_row.push(fmt_count(total_elements));
    }
    total_row.push(String::new()); // Holder Instances (already empty)
    total_row.push(String::new()); // Sharing (no total)
    total_row.push(format_bytes(total_retained));
    if graphs {
        total_row.push(String::new());
    }
    t.row(total_row);
    t.render(out);
    out.push('\n');
}

/// Largest individual collection instances: a combined ranking plus per-kind
/// sub-tables. Retained/owner/value columns render only when present (filled
/// under `--collections`). Shared by md and md-graphs; `graphs` adds a
/// proportional bar on Retained (or Elements when retained is absent).
pub(crate) fn render_biggest_collections(
    b: &Option<BiggestCollections>,
    graphs: bool,
    out: &mut String,
) {
    let Some(b) = b else {
        return;
    };
    out.push_str("## Biggest Collections\n\n");
    out.push_str(
        "_The largest individual collection instances. Owner is the primary incoming \
         `Class#field`; value type is the dominant runtime element type of the \
         backing array (the direct element, not the logical key/value — for a \
         `Map<K,V>` this is often `Entry` or `Object`, not `V`). \
         Owner/retained/value columns require `--collections`. Consider replacing \
         over-allocated maps/lists with right-sized or lazy alternatives._\n\n",
    );

    // When per-kind breakdown is available show it directly (avoids listing every
    // row twice — once in Combined and again in By Kind). Fall back to Combined
    // only when there are no by-kind tables.
    if b.by_kind.is_empty() {
        render_biggest_collection_table(&b.combined, "Combined", graphs, out);
    } else {
        // Emit a compact combined summary (total elements + retained) without
        // repeating every row, then show the full per-kind breakdown.
        if !b.combined.is_empty() {
            let total_elements: u64 = b.combined.iter().map(|r| r.elements).sum();
            let total_retained: u64 = b.combined.iter().filter_map(|r| r.retained).sum();
            let n_kinds = b.by_kind.len();
            out.push_str(&format!(
                "_Total: {} elements across {} collection kind(s)",
                fmt_count(total_elements),
                n_kinds,
            ));
            if total_retained > 0 {
                out.push_str(&format!(", {} retained", format_bytes(total_retained)));
            }
            out.push_str(". See per-kind breakdown below._\n\n");
        }
        for k in &b.by_kind {
            let title = format!("By Kind — {}", k.kind);
            render_biggest_collection_table(&k.rows, &title, graphs, out);
        }
    }
    if b.truncated {
        out.push_str("_Collection value tally was truncated; ranking is a bounded sample._\n\n");
    }
}

/// One biggest-collections sub-table. Columns adapt: retained/owner/value shown
/// only when at least one row carries them.
fn render_biggest_collection_table(
    rows: &[BiggestCollectionRow],
    title: &str,
    graphs: bool,
    out: &mut String,
) {
    use crate::md::{Align, Table, bar};
    out.push_str(&format!("### {title}\n\n"));
    if rows.is_empty() {
        out.push_str("_No data for this section._\n\n");
        return;
    }
    let has_retained = rows.iter().any(|r| r.retained.is_some());
    let has_owner = rows.iter().any(|r| r.owner.is_some());
    let has_breakdown = rows.iter().any(|r| !r.value_type_breakdown.is_empty());
    // Drop the single dominant_value_type column when the breakdown is present —
    // it duplicates the breakdown's lead entry on every row.
    let has_value = !has_breakdown && rows.iter().any(|r| r.dominant_value_type.is_some());

    let mut headers: Vec<&str> = vec!["Kind", "Container Class", "Elements"];
    let mut aligns = vec![Align::Left, Align::Left, Align::Right];
    if has_value {
        headers.push("Value Type");
        aligns.push(Align::Left);
    }
    if has_breakdown {
        headers.push("Value Types (top)");
        aligns.push(Align::Left);
    }
    if has_owner {
        headers.push("Owner (Class#field)");
        aligns.push(Align::Left);
    }
    if has_retained {
        headers.push("Retained");
        aligns.push(Align::Right);
    }
    if graphs {
        headers.push("");
        aligns.push(Align::Left);
    }

    let ret_max = rows.iter().filter_map(|r| r.retained).max().unwrap_or(0);
    let el_max = rows.iter().map(|r| r.elements).max().unwrap_or(0);

    let mut t = Table::new(&headers, &aligns);
    let total_elements: u64 = rows.iter().map(|r| r.elements).sum();
    let total_retained: u64 = rows.iter().filter_map(|r| r.retained).sum();

    // Coalesce consecutive identical (kind, class, elements, owner, retained) rows.
    let mut i = 0;
    while i < rows.len() {
        let r = &rows[i];
        let count = rows[i..]
            .iter()
            .take_while(|x| {
                x.kind == r.kind
                    && x.container_class == r.container_class
                    && x.elements == r.elements
                    && x.owner == r.owner
                    && x.retained == r.retained
            })
            .count();
        let class_cell = if count > 1 {
            format!("`{}` ×{}", r.container_class, fmt_count(count as u64))
        } else {
            format!("`{}`", r.container_class)
        };
        let elements_cell = if count > 1 {
            format!("{} each", fmt_count(r.elements))
        } else {
            fmt_count(r.elements)
        };
        let mut row = vec![r.kind.clone(), class_cell, elements_cell];
        if has_value {
            row.push(match &r.dominant_value_type {
                Some(v) => format!("`{v}`"),
                None => "—".to_string(),
            });
        }
        if has_breakdown {
            row.push(if r.value_type_breakdown.is_empty() {
                "—".to_string()
            } else {
                r.value_type_breakdown
                    .iter()
                    .map(|s| format!("`{}` ×{}", s.type_name, fmt_count(s.count)))
                    .collect::<Vec<_>>()
                    .join(", ")
            });
        }
        if has_owner {
            row.push(match &r.owner {
                Some(o) => format!("`{o}`"),
                None => "—".to_string(),
            });
        }
        if has_retained {
            row.push(match r.retained {
                Some(x) => format_bytes(x),
                None => "—".to_string(),
            });
        }
        if graphs {
            let (v, m) = if has_retained {
                (r.retained.unwrap_or(0), ret_max)
            } else {
                (r.elements, el_max)
            };
            row.push(bar(v, m, render_graphs::GRAPH_BAR_WIDTH));
        }
        t.row(row);
        i += count;
    }
    let mut total_row = vec![
        "**Total**".to_string(),
        String::new(),
        fmt_count(total_elements),
    ];
    if has_value {
        total_row.push(String::new());
    }
    if has_breakdown {
        total_row.push(String::new());
    }
    if has_owner {
        total_row.push(String::new());
    }
    if has_retained {
        total_row.push(format!("**{}**", format_bytes(total_retained)));
    }
    if graphs {
        total_row.push(String::new());
    }
    t.row(total_row);
    t.render(out);
    out.push('\n');
}

/// Global per-collection-class value-type breakdown. Present only under
/// `--collections`. Each row: collection class, instance count, total element
/// slots, and the top runtime element types (as `Type ×count`). No graphs bar.
pub(crate) fn render_collection_contents(
    c: &Option<CollectionContents>,
    _graphs: bool,
    out: &mut String,
) {
    use crate::md::{Align, Table};
    let Some(c) = c else {
        return;
    };
    out.push_str("## Collection Contents by Type\n\n");
    out.push_str(
        "_Element types stored in each collection class, summed across all instances. \
         Spot unexpected or boxed value types that could be replaced with primitive arrays \
         or more specific collections. Requires `--collections`._\n\n",
    );
    if c.rows.is_empty() {
        out.push_str("_No collection-contents data found._\n\n");
        return;
    }
    let mut t = Table::new(
        &[
            "Collection Class",
            "Instances",
            "Total Values",
            "Top Value Types",
        ],
        &[Align::Left, Align::Right, Align::Right, Align::Left],
    );
    for r in &c.rows {
        let types = if r.top_value_types.is_empty() {
            "—".to_string()
        } else {
            r.top_value_types
                .iter()
                .map(|s| format!("`{}` ×{}", s.type_name, fmt_count(s.count)))
                .collect::<Vec<_>>()
                .join(", ")
        };
        t.row(vec![
            format!("`{}`", r.collection_class),
            fmt_count(r.instances),
            fmt_count(r.total_values),
            types,
        ]);
    }
    t.render(out);
    out.push('\n');
    if c.truncated {
        out.push_str("_Truncated; a bounded sample of collection classes is shown._\n\n");
    }
}
/// referent histograms plus (where present) an approximate only-weakly-retained
/// breakdown. Shared by plain md and md-graphs; when `graphs` is set an extra
/// proportional bar column is appended on Objects. Emits the heading + a
/// fallback line even when no references are present so the structure stays
/// stable.
pub(crate) fn render_references(rf: &ReferencesAnalysis, graphs: bool, out: &mut String) {
    use crate::md::{Align, Table, bar};
    out.push_str("## References\n\n");
    out.push_str("_Soft, weak, and phantom references — referents, retention status, and null-referent counts._\n\n");

    if rf.soft.is_none() && rf.weak.is_none() && rf.phantom.is_none() {
        out.push_str("_No soft, weak, or phantom references found._\n\n");
        return;
    }

    let render_class_table = |rows: &[RefStatClassRow], out: &mut String| {
        const REF_CLASS_CAP: usize = 20;
        let shown = rows.len().min(REF_CLASS_CAP);
        let displayed = &rows[..shown];
        let ret_max = displayed.iter().map(|r| r.retained).max().unwrap_or(0);
        let mut headers: Vec<&str> = vec!["Class", "Objects", "Shallow", "Retained"];
        let mut aligns = vec![Align::Left, Align::Right, Align::Right, Align::Right];
        if graphs {
            headers.push("");
            aligns.push(Align::Left);
        }
        let mut t = Table::new(&headers, &aligns);
        for r in displayed {
            let mut row = vec![
                format!("`{}`", r.pretty_class),
                fmt_count(r.objects),
                format_bytes(r.shallow),
                format_bytes(r.retained),
            ];
            if graphs {
                row.push(bar(r.retained, ret_max, render_graphs::GRAPH_BAR_WIDTH));
            }
            t.row(row);
        }
        t.render(out);
        if rows.len() > REF_CLASS_CAP {
            let hidden = rows.len() - REF_CLASS_CAP;
            let tail_obj: u64 = rows[REF_CLASS_CAP..].iter().map(|r| r.objects).sum();
            let tail_sh: u64 = rows[REF_CLASS_CAP..].iter().map(|r| r.shallow).sum();
            let tail_ret: u64 = rows[REF_CLASS_CAP..].iter().map(|r| r.retained).sum();
            out.push_str(&format!(
                "_… {} more classes ({} objects, {} shallow, {} retained)._\n",
                fmt_count(hidden as u64),
                fmt_count(tail_obj),
                format_bytes(tail_sh),
                format_bytes(tail_ret),
            ));
        }
        out.push('\n');
    };

    for stats in [&rf.soft, &rf.weak, &rf.phantom].into_iter().flatten() {
        out.push_str(&format!("### {} References\n\n", stats.kind));
        let kind_caption = match stats.kind.as_str() {
            "Soft" => {
                "_Soft references keep objects alive until the JVM needs memory — cleared \
under GC pressure. A large soft-referenced heap signals an oversized cache; cap it with a \
max-entries limit or switch to an explicit bounded cache (e.g. Caffeine)._"
            }
            "Weak" => {
                "_Weak references let GC claim referents — reachable only via weak chains, \
reclaimed at any collection. Large counts are usually benign, but a growing count can \
indicate ThreadLocal leaks or listener registries not deregistering._"
            }
            "Phantom" => {
                "_Phantom references track objects in cleanup pipelines for native resource \
release. A large backlog signals a stalled or overloaded ReferenceQueue processor, or \
indicates native resources (file handles, off-heap buffers) not being released promptly._"
            }
            _ => "",
        };
        if !kind_caption.is_empty() {
            out.push_str(kind_caption);
            out.push_str("\n\n");
        }
        out.push_str(&format!(
            "_{} reference instances._\n\n",
            fmt_count(stats.reference_instances),
        ));
        out.push_str("#### Referent Classes\n\n");
        render_class_table(&stats.referent_histogram, out);
        out.push_str("#### Only Weakly Retained\n\n");
        let only_caption = match stats.kind.as_str() {
            "Soft" => {
                "_Referents reachable only through soft references — no strong path. GC clears these under memory pressure._"
            }
            "Weak" => {
                "_Referents reachable only through weak references — no strong or soft path. GC can reclaim them at any collection._"
            }
            "Phantom" => {
                "_Referents reachable only through phantom references — queued for post-cleanup resource release._"
            }
            _ => "_Objects reachable only via this reference kind — no incoming strong reference._",
        };
        out.push_str(only_caption);
        out.push_str("\n\n");
        if stats.only_weakly_retained.is_empty() {
            out.push_str(
                "_None found — no objects are exclusively reachable via this reference kind._\n\n",
            );
        } else {
            render_class_table(&stats.only_weakly_retained, out);
        }
    }
}

/// Render the always-on "Unreachable Objects" section: a per-class histogram
/// plus a by-kind composition table, sorted by shallow descending and capped.
/// Shared by plain md and md-graphs; when `graphs` is set, proportional bar
/// columns are appended. Emits the heading + a fallback italic line when empty.
pub(crate) fn render_unreachable_histogram(o: &SystemOverview, graphs: bool, out: &mut String) {
    use crate::md::{Align, Table, bar};
    out.push_str("## Unreachable Objects\n\n");
    if o.unreachable_histogram.is_empty() {
        out.push_str("_No unreachable objects._\n\n");
        return;
    }
    out.push_str(&format!(
        "_{} unreachable objects, {} shallow heap. \
         Top {} classes by shallow heap._\n\n",
        fmt_count(o.unreachable_count),
        format_bytes(o.unreachable_shallow),
        UNREACHABLE_HISTOGRAM_CAP,
    ));
    // Add context about what unreachable means and when it's a concern.
    let total = o.total_shallow + o.unreachable_shallow;
    let unreachable_pct = if total > 0 {
        o.unreachable_shallow as f64 / total as f64 * 100.0
    } else {
        0.0
    };
    if unreachable_pct >= 5.0 {
        out.push_str(&format!(
            "_Unreachable objects are eligible for collection but have not yet been reclaimed. \
At {} of heap total (reachable + unreachable) this is elevated — the dump was likely taken \
before a full GC cycle completed. GC reclaims this memory automatically; it is not a leak. \
Confirm: trigger a full GC (`jcmd <pid> GC.run`) then re-dump; if the count drops, \
it was pre-GC garbage._\n\n",
            fmt_pct(unreachable_pct)
        ));
    } else {
        out.push_str(
            "_Unreachable objects are eligible for collection but have not yet been reclaimed. \
A small unreachable heap (< 5% of heap total) is normal between GC cycles._\n\n",
        );
    }
    // Composition by object kind (mirrors System Overview heap composition).
    if o.unreachable_composition.by_kind.len() > 1 {
        let mut headers: Vec<&str> = vec!["Kind", "Objects", "Shallow"];
        let mut aligns = vec![Align::Left, Align::Right, Align::Right];
        if graphs {
            headers.push("");
            aligns.push(Align::Left);
        }
        let sh_max = o
            .unreachable_composition
            .by_kind
            .iter()
            .map(|k| k.shallow_heap)
            .max()
            .unwrap_or(0);
        let mut t = Table::new(&headers, &aligns);
        for k in &o.unreachable_composition.by_kind {
            let mut row = vec![
                k.kind.clone(),
                fmt_count(k.objects),
                format_bytes(k.shallow_heap),
            ];
            if graphs {
                row.push(bar(k.shallow_heap, sh_max, render_graphs::GRAPH_BAR_WIDTH));
            }
            t.row(row);
        }
        t.render(out);
        out.push('\n');
    }
    // Per-class histogram.
    let sh_max_hist = o
        .unreachable_histogram
        .iter()
        .map(|r| r.shallow)
        .max()
        .unwrap_or(0);
    let mut headers: Vec<&str> = vec!["Class", "Objects", "Shallow", "Retained"];
    let mut aligns = vec![Align::Left, Align::Right, Align::Right, Align::Right];
    if graphs {
        headers.push("");
        aligns.push(Align::Left);
    }
    out.push_str("_Shallow heap is additive; Retained sets overlap (nested subtrees are counted once per ancestor)._\n\n");
    let mut t = Table::new(&headers, &aligns);
    for r in &o.unreachable_histogram {
        let mut row = vec![
            format!("`{}`", r.pretty_class),
            fmt_count(r.objects),
            format_bytes(r.shallow),
            format_bytes(r.retained),
        ];
        if graphs {
            row.push(bar(r.shallow, sh_max_hist, render_graphs::GRAPH_BAR_WIDTH));
        }
        t.row(row);
    }
    t.render(out);
    out.push('\n');
    // Garbage-root dominator trees.
    render_garbage_root_trees(&o.unreachable_garbage_roots, graphs, out);
}

/// Render the top garbage-root dominator subtrees as indented ASCII trees.
/// Each line: `prefix class — retained (N objects)`, optionally with a
/// retained ASCII bar when `graphs` is true.
fn render_garbage_root_trees(roots: &[UnreachableGarbageRoot], graphs: bool, out: &mut String) {
    if roots.is_empty() {
        return;
    }
    let retained_max = roots.iter().map(|r| r.retained).max().unwrap_or(0);
    out.push_str("### Garbage-Root Dominator Trees\n\n");
    out.push_str(
        "_Top garbage-root subtrees by retained heap (unreachable objects \
                  with no reachable predecessor). Depth capped._\n\n",
    );
    for (i, root) in roots.iter().enumerate() {
        let label = if graphs {
            use crate::md::bar;
            format!(
                "**{}** — {} ({} {} in subtree) {}",
                root.pretty_class,
                format_bytes(root.retained),
                fmt_count(root.objects),
                plural_objects(root.objects),
                bar(root.retained, retained_max, render_graphs::GRAPH_BAR_WIDTH),
            )
        } else {
            format!(
                "**{}** — {} ({} {} in subtree)",
                root.pretty_class,
                format_bytes(root.retained),
                fmt_count(root.objects),
                plural_objects(root.objects),
            )
        };
        out.push_str(&format!("{}. {}\n", i + 1, label));
        render_garbage_root_node(&root.children, "   ", graphs, retained_max, out);
        out.push('\n');
    }
}

fn render_garbage_root_node(
    nodes: &[UnreachableGarbageRoot],
    prefix: &str,
    graphs: bool,
    retained_max: u64,
    out: &mut String,
) {
    for (i, node) in nodes.iter().enumerate() {
        let is_last = i == nodes.len() - 1;
        let connector = if is_last { "└─ " } else { "├─ " };
        let line = if graphs {
            use crate::md::bar;
            format!(
                "{}{}{} — {} {}\n",
                prefix,
                connector,
                node.pretty_class,
                format_bytes(node.retained),
                bar(node.retained, retained_max, render_graphs::GRAPH_BAR_WIDTH),
            )
        } else {
            format!(
                "{}{}{} — {}\n",
                prefix,
                connector,
                node.pretty_class,
                format_bytes(node.retained),
            )
        };
        out.push_str(&line);
        let child_prefix = format!("{}{}  ", prefix, if is_last { "   " } else { "│  " });
        render_garbage_root_node(&node.children, &child_prefix, graphs, retained_max, out);
    }
}

/// Render the always-on "Dominator Analysis" section: two dominator-tree
/// sub-views. "Big Drops" lists dominators where retained heap concentrates
/// (retained minus the largest single child); "Immediate Dominators" rolls up
/// the immediately-dominated objects by their dominator's class. Shared by plain
/// md and md-graphs; when `graphs` is set, a proportional bar column is appended
/// on Drop (big drops) and on Dominated Shallow (immediate dominators). Emits the
/// headings + fallback italic lines even when empty so the structure stays stable.
pub(crate) fn render_dominator_analysis(d: &DominatorAnalysis, graphs: bool, out: &mut String) {
    use crate::md::{Align, Table, bar};
    out.push_str("## Dominator Analysis\n\n");
    out.push_str(
        "_Instances ranked by retained heap. An object _dominates_ another if every path \
from a GC root to that object passes through it — making the dominator unreachable reclaims \
everything it dominates._\n\n",
    );

    // ---- Big Drops ----
    out.push_str("### Big Drops\n\n");
    let threshold_mb = d.big_drops.threshold as f64 / (1024.0 * 1024.0);
    out.push_str(&format!(
        "_Objects retaining far more than their largest single child — memory held directly \
in the object or spread across many small dominated children. \
Drop = object retained − largest child retained (memory reclaimed if this object became unreachable, \
net of what the biggest child already accounts for). \
Threshold {:.1} MB (1% of reachable heap). \
Multiple rows with the same class are distinct objects._\n\n",
        threshold_mb,
    ));
    if d.big_drops.rows.is_empty() {
        out.push_str("_No significant drops._\n\n");
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
            "#",
            "Retained",
            "Largest Child",
            "Child Retained",
            "Drop",
        ];
        let mut aligns = vec![
            Align::Left,
            Align::Right,
            Align::Right,
            Align::Left,
            Align::Right,
            Align::Right,
        ];
        if graphs {
            headers.push("");
            aligns.push(Align::Left);
        }
        let total_retained: u64 = d.big_drops.rows.iter().map(|r| r.retained).sum();
        let total_child_ret: u64 = d
            .big_drops
            .rows
            .iter()
            .map(|r| r.largest_child_retained)
            .sum();
        let total_drop: u64 = d.big_drops.rows.iter().map(|r| r.drop_bytes).sum();
        let mut t = Table::new(&headers, &aligns);
        // Group consecutive rows sharing the same (class, drop_bytes) into a single
        // "×N" row — identical entries come from multiple objects of the same type
        // at the same drop level and repeat without adding new information.
        let rows = &d.big_drops.rows;
        let mut i = 0;
        while i < rows.len() {
            let r = &rows[i];
            let count = rows[i..]
                .iter()
                .take_while(|x| x.display_class == r.display_class && x.drop_bytes == r.drop_bytes)
                .count();
            let child = if r.largest_child_class.is_empty() {
                "—".to_string()
            } else {
                format!("`{}`", r.largest_child_class)
            };
            let count_cell = if count > 1 {
                format!("×{}", fmt_count(count as u64))
            } else {
                r.obj_index_1based.to_string()
            };
            let mut row = vec![
                format!("`{}`", r.display_class),
                count_cell,
                format_bytes(r.retained),
                child,
                format_bytes(r.largest_child_retained),
                format_bytes(r.drop_bytes),
            ];
            if graphs {
                row.push(bar(r.drop_bytes, drop_max, render_graphs::GRAPH_BAR_WIDTH));
            }
            t.row(row);
            i += count;
        }
        let mut total_row = vec![
            "**Total**".to_string(),
            String::new(),
            format!("**{}**", format_bytes(total_retained)),
            String::new(),
            format!("**{}**", format_bytes(total_child_ret)),
            format!("**{}**", format_bytes(total_drop)),
        ];
        if graphs {
            total_row.push(String::new());
        }
        t.row(total_row);
        t.render(out);
        out.push('\n');
    }

    // ---- Immediate Dominators ----
    out.push_str("### Immediate Dominators\n\n");
    out.push_str(
        "_One row per dominator class: how many other objects it immediately dominates \
         and the total shallow heap of those dominated objects. A large dominated-shallow \
         figure means instances of that class are collectively gating large portions of \
         the live heap — making them unreachable would allow that memory to be reclaimed._\n\n",
    );
    if d.immediate_dominators.rows.is_empty() {
        out.push_str("_No immediate dominators._\n\n");
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
        let total_dom_count: u64 = d
            .immediate_dominators
            .rows
            .iter()
            .map(|r| r.dominator_count)
            .sum();
        let total_dmd_count: u64 = d
            .immediate_dominators
            .rows
            .iter()
            .map(|r| r.dominated_count)
            .sum();
        let total_dom_shallow: u64 = d
            .immediate_dominators
            .rows
            .iter()
            .map(|r| r.dominator_shallow)
            .sum();
        let total_dmd_shallow: u64 = d
            .immediate_dominators
            .rows
            .iter()
            .map(|r| r.dominated_shallow)
            .sum();
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
        let mut total_row = vec![
            "**Total**".to_string(),
            format!("**{}**", fmt_count(total_dom_count)),
            format!("**{}**", fmt_count(total_dmd_count)),
            format!("**{}**", format_bytes(total_dom_shallow)),
            format!("**{}**", format_bytes(total_dmd_shallow)),
        ];
        if graphs {
            total_row.push(String::new());
        }
        t.row(total_row);
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
    out.push_str("**Dominator chain to GC root:**\n\n");
    if path.len() == 1 {
        let step = &path[0];
        let mut line = format!(
            "1. `{}` ({})",
            step.display_class,
            format_bytes(step.retained)
        );
        if let Some(label) = &step.root_type_label {
            line.push_str(&format!(" — GC root: {label} (this object is directly held by a GC root; no intermediate chain)"));
        }
        line.push('\n');
        out.push_str(&line);
        out.push('\n');
        return;
    }
    let last = path.len() - 1;
    for (i, step) in path.iter().enumerate() {
        let class_label = if let Some(f) = &step.field_edge {
            format!(".{f} → `{}`", step.display_class)
        } else {
            format!("`{}`", step.display_class)
        };
        let mut line = format!(
            "{}. {} ({})",
            i + 1,
            class_label,
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
/// spaces per level. Sibling nodes with identical (class, shallow, retained)
/// are collapsed into a single "N×" line to reduce noise in deep uniform trees.
/// Depth and breadth are capped (depth ≤ 5, breadth ≤ 5 per node) to keep
/// the rendered output scannable; a tail note is added when rows are omitted.
fn render_dom_tree_plain(root: &DomTreeNode, out: &mut String) {
    out.push_str("**Dominator subtree:**\n\n");
    render_dom_node(root, 0, out);
    out.push('\n');
}

const DOM_TREE_MAX_DEPTH: usize = 5;
const DOM_TREE_MAX_BREADTH: usize = 5;

fn render_dom_node(node: &DomTreeNode, depth: usize, out: &mut String) {
    let indent = "  ".repeat(depth);
    out.push_str(&format!(
        "{}- `{}` (shallow {}, retained {})\n",
        indent,
        node.display_class,
        format_bytes(node.shallow),
        format_bytes(node.retained),
    ));
    if depth >= DOM_TREE_MAX_DEPTH || node.children.is_empty() {
        if depth >= DOM_TREE_MAX_DEPTH && !node.children.is_empty() {
            let child_indent = "  ".repeat(depth + 1);
            out.push_str(&format!(
                "{}_… ({} deeper — full data in JSON)_\n",
                child_indent,
                node.children.len(),
            ));
        }
        return;
    }
    // Group children by (class, shallow, retained) and collapse duplicates.
    let mut i = 0;
    let mut shown = 0usize;
    while i < node.children.len() && shown < DOM_TREE_MAX_BREADTH {
        let child = &node.children[i];
        let key = (&child.display_class, child.shallow, child.retained);
        let mut count = 1usize;
        while i + count < node.children.len() {
            let next = &node.children[i + count];
            if (&next.display_class, next.shallow, next.retained) == key {
                count += 1;
            } else {
                break;
            }
        }
        if count > 1 {
            // Collapsed group: emit a summary line, recurse into first child's children.
            let child_indent = "  ".repeat(depth + 1);
            out.push_str(&format!(
                "{}- `{}` ×{} (shallow {}, retained {} each)\n",
                child_indent,
                child.display_class,
                count,
                format_bytes(child.shallow),
                format_bytes(child.retained),
            ));
            // Recurse into the children of the first representative.
            for grandchild in &child.children {
                render_dom_node(grandchild, depth + 2, out);
            }
        } else {
            render_dom_node(child, depth + 1, out);
        }
        i += count;
        shown += 1;
    }
    let remaining = node.children.len().saturating_sub(i);
    if remaining > 0 {
        let child_indent = "  ".repeat(depth + 1);
        out.push_str(&format!(
            "{}_… ({} more siblings — full data in JSON)_\n",
            child_indent, remaining,
        ));
    }
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
        let class_label = if let Some(f) = &node.field_edge {
            format!(".{f} → `{}`", node.display_class)
        } else {
            format!("`{}`", node.display_class)
        };
        let mut line = format!(
            "{}- {} ({} {}, retained {})",
            indent,
            class_label,
            fmt_count(node.object_count),
            plural_objects(node.object_count),
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
    out.push_str(
        "_Objects grouped by the stack trace that allocated them — shows where heap was \
created, not necessarily what is keeping it alive. Only available when the dump was \
captured with the HPROF agent (JDK 8 and earlier). Each site is a candidate to \
allocate less by pooling, caching, or deferring construction._\n\n",
    );
    if !a.traces_present {
        out.push_str(
            "_Allocation tracking not captured. This requires the HPROF agent \
(`-agentlib:hprof=heap=dump,depth=8`), which was removed in JDK 9. Standard \
`jmap`/`jcmd` dumps do not include per-site allocation stacks._\n\n",
        );
        return;
    }
    // When all sites lack frame data (no named frames), the section adds no actionable
    // information. Show a brief note instead of a table of "serial N" entries.
    let any_frames = a.sites.iter().any(|s| !s.frames.is_empty());
    if !any_frames {
        out.push_str(
            "_Allocation-site records are present but contain no per-frame data. \
The HPROF agent must be invoked with `depth=8` or higher to record method-level \
allocation stacks: `-agentlib:hprof=heap=dump,depth=8`._\n\n",
        );
        return;
    }
    let max = a.sites.iter().map(|s| s.object_count).max().unwrap_or(0);
    use crate::md::{Align, Table, bar};
    let mut t = if graphs {
        Table::new(
            &["Stack", "Objects", "Shallow", ""],
            &[Align::Left, Align::Right, Align::Right, Align::Left],
        )
    } else {
        Table::new(
            &["Stack", "Objects", "Shallow"],
            &[Align::Left, Align::Right, Align::Right],
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
                bar(site.object_count, max, GRAPH_BAR_WIDTH),
            ]);
        } else {
            t.row([
                stack,
                fmt_count(site.object_count),
                format_bytes(site.shallow_total),
            ]);
        }
    }
    t.render(out);
    out.push('\n');
}

pub(crate) fn render_duplicate_prim_arrays(
    out: &mut String,
    d: &Option<crate::pass2::DupPrimArrays>,
) {
    use crate::md::{Align, Table};
    out.push_str("### Duplicate Primitive Arrays (approximate)\n\n");
    let d = match d {
        None => {
            out.push_str(
                "_Duplicate primitive-array analysis not run (pass `--find-duplicates`)._\n\n",
            );
            return;
        }
        Some(d) => d,
    };
    out.push_str(
        "_Primitive arrays with identical content — each group wastes memory holding \
redundant copies. Replace with a shared `static final` constant, use a canonical-instance \
registry, or intern at creation time. \
Deduplication is approximate (64-bit hash; rare collisions possible)._\n\n",
    );
    out.push_str(&format!(
        "- Approx wasted bytes: {}\n\n",
        format_bytes(d.total_wasted_bytes)
    ));
    if !d.rows.is_empty() {
        out.push_str("#### Waste by Array Element Type\n\n");
        let mut t = Table::new(
            &["#", "Array type", "Dup groups", "Wasted"],
            &[Align::Right, Align::Left, Align::Right, Align::Right],
        );
        for (i, row) in d.rows.iter().enumerate() {
            t.row([
                format!("{}", i + 1),
                format!("`{}`", row.array_class),
                fmt_count(row.duplicated_groups),
                format_bytes(row.wasted_bytes),
            ]);
        }
        t.render(out);
        out.push('\n');
    }
    if !d.top_array_holders.is_empty() {
        out.push_str("#### Classes Holding the Most Duplicate Arrays\n\n");
        let mut t = Table::new(
            &["#", "Class", "Array refs"],
            &[Align::Right, Align::Left, Align::Right],
        );
        for (i, h) in d.top_array_holders.iter().enumerate() {
            t.row([
                format!("{}", i + 1),
                format!("`{}`", h.class_name),
                fmt_count(h.array_refs),
            ]);
        }
        t.render(out);
        out.push('\n');
    }
}

pub(crate) fn render_boxed_numbers(
    out: &mut String,
    rows: &[crate::report::model::BoxedNumberRow],
    holders: &[crate::report::model::BoxedNumberHolder],
    total_shallow: u64,
) {
    use crate::md::{Align, Table};
    if rows.is_empty() {
        return;
    }
    out.push_str("### Boxed Numbers\n\n");
    out.push_str(
        "_Heap consumed by `Integer`, `Long`, `Double`, and other boxed wrapper types. \
Each boxed value costs 16–24 bytes (12-byte object header + primitive field, padded \
to 8-byte boundary) versus 4–8 bytes for an unboxed primitive. Replacing with \
primitive fields or `int[]`/`long[]` arrays eliminates the per-object header._\n\n",
    );
    let mut t = Table::new(
        &[
            "#",
            "Class",
            "Instances",
            "Total Shallow",
            "% of Heap",
            "Avg Size",
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
    for (i, row) in rows.iter().enumerate() {
        let pct = if total_shallow > 0 {
            fmt_pct(row.pct_of_heap_bp as f64 / 100.0)
        } else {
            "—".to_string()
        };
        t.row([
            format!("{}", i + 1),
            format!("`{}`", row.pretty_class),
            fmt_count(row.instances),
            format_bytes(row.total_shallow),
            pct,
            format_bytes(row.avg_shallow),
        ]);
    }
    t.render(out);
    out.push('\n');
    if !holders.is_empty() {
        out.push_str("#### Classes Holding the Most Boxed-Number References\n\n");
        let mut t = Table::new(
            &["#", "Class", "Boxed refs"],
            &[Align::Right, Align::Left, Align::Right],
        );
        for (i, h) in holders.iter().enumerate() {
            t.row([
                format!("{}", i + 1),
                format!("`{}`", h.class_name),
                fmt_count(h.boxed_refs),
            ]);
        }
        t.render(out);
        out.push('\n');
    }
}

pub(crate) fn render_header_overhead(
    out: &mut String,
    rows: &[crate::report::model::HeaderOverheadRow],
) {
    use crate::md::{Align, Table};
    if rows.is_empty() {
        return;
    }
    out.push_str("### Object Header Overhead\n\n");
    out.push_str(
        "_Classes where object headers (12 bytes with compressed OOPs, 16 without) \
         consume a large share of shallow heap. The practical action is to reduce \
         object *count*: merge small objects, use primitive arrays instead of boxed \
         wrappers, or replace fine-grained instances with a flat array of fields. \
         Value types (Project Valhalla) eliminate headers entirely._\n\n",
    );
    let mut t = Table::new(
        &[
            "#",
            "Class",
            "Instances",
            "Hdr/obj",
            "Total Headers",
            "Hdr %",
            "Avg Size",
        ],
        &[
            Align::Right,
            Align::Left,
            Align::Right,
            Align::Right,
            Align::Right,
            Align::Right,
            Align::Right,
        ],
    );
    for (i, row) in rows.iter().enumerate() {
        t.row([
            format!("{}", i + 1),
            format!("`{}`", row.pretty_class),
            fmt_count(row.instances),
            format!("{} B", row.header_bytes),
            format_bytes(row.total_header_bytes),
            fmt_pct(row.header_pct_of_shallow_bp as f64 / 100.0),
            format_bytes(row.avg_shallow),
        ]);
    }
    t.render(out);
    out.push('\n');
}

#[cfg(test)]
mod tests {
    use super::{fmt_query_value, render_custom_queries};
    use crate::query::model::{QueryColumn, QueryResult, QueryValue};

    fn col(name: &str) -> QueryColumn {
        QueryColumn { name: name.into() }
    }

    #[test]
    fn empty_queries_produce_empty_string() {
        let mut out = String::new();
        render_custom_queries(&[], &mut out);
        assert_eq!(out, "", "the is_empty gate must emit nothing");
    }

    #[test]
    fn normal_result_renders_full_table() {
        let q = QueryResult {
            name: "q1".into(),
            oql: "SELECT a, b FROM C".into(),
            columns: vec![col("a"), col("b")],
            rows: vec![
                vec![QueryValue::Int(1), QueryValue::Str("x".into())],
                vec![QueryValue::Int(2), QueryValue::Str("y".into())],
            ],
            row_count: 2,
            truncated: false,
            error: None,
            note: None,
            viz: None,
            elapsed_ms: None,
        };
        let mut out = String::new();
        render_custom_queries(std::slice::from_ref(&q), &mut out);
        assert!(
            out.contains("## Custom Queries"),
            "section heading missing: {out}"
        );
        assert!(out.contains("### q1"), "query name heading missing: {out}");
        // Fenced OQL block.
        assert!(
            out.contains("```\nSELECT a, b FROM C\n```"),
            "fenced OQL block missing: {out}"
        );
        // Header row + separator.
        assert!(out.contains("| a | b |"), "header row missing: {out}");
        assert!(
            out.contains("| --- | --- |"),
            "separator row missing: {out}"
        );
        // Both data rows.
        assert!(out.contains("| 1 | x |"), "first data row missing: {out}");
        assert!(out.contains("| 2 | y |"), "second data row missing: {out}");
        // Footer.
        assert!(
            out.contains("_2 row(s)_"),
            "row-count footer missing: {out}"
        );
        assert!(
            !out.contains("truncated"),
            "non-truncated result must not say truncated: {out}"
        );
    }

    #[test]
    fn error_result_shows_error_and_no_table() {
        let q = QueryResult {
            name: "bad".into(),
            oql: "SELECT bogus".into(),
            columns: vec![col("x")],
            rows: vec![],
            row_count: 0,
            truncated: false,
            error: Some("parse failed near 'bogus'".into()),
            note: None,
            viz: None,
            elapsed_ms: None,
        };
        let mut out = String::new();
        render_custom_queries(std::slice::from_ref(&q), &mut out);
        assert!(
            out.contains("**Error:** parse failed near 'bogus'"),
            "error line missing: {out}"
        );
        // No table header/separator emitted for the errored query.
        assert!(
            !out.contains("| x |"),
            "errored query must not emit a header row: {out}"
        );
        assert!(
            !out.contains("| --- |"),
            "errored query must not emit a separator row: {out}"
        );
    }

    #[test]
    fn truncated_result_footer_notes_truncation() {
        let q = QueryResult {
            name: "big".into(),
            oql: "SELECT * FROM C".into(),
            columns: vec![col("v")],
            rows: vec![vec![QueryValue::Int(1)]],
            row_count: 5000,
            truncated: true,
            error: None,
            note: None,
            viz: None,
            elapsed_ms: None,
        };
        let mut out = String::new();
        render_custom_queries(std::slice::from_ref(&q), &mut out);
        assert!(
            out.contains("_5000 row(s), truncated_"),
            "truncated footer missing: {out}"
        );
    }

    #[test]
    fn note_line_rendered_when_present() {
        let q = QueryResult {
            name: "noted".into(),
            oql: "SELECT path(a, b) FROM C".into(),
            columns: vec![col("v")],
            rows: vec![vec![QueryValue::Int(1)]],
            row_count: 1,
            truncated: false,
            error: None,
            note: Some("edge retention capped at depth 5".into()),
            viz: None,
            elapsed_ms: None,
        };
        let mut out = String::new();
        render_custom_queries(std::slice::from_ref(&q), &mut out);
        assert!(
            out.contains("_Note: edge retention capped at depth 5_"),
            "note advisory line missing: {out}"
        );
    }

    #[test]
    fn note_line_omitted_when_absent() {
        let q = QueryResult {
            name: "plain".into(),
            oql: "SELECT * FROM C".into(),
            columns: vec![col("v")],
            rows: vec![vec![QueryValue::Int(1)]],
            row_count: 1,
            truncated: false,
            error: None,
            note: None,
            viz: None,
            elapsed_ms: None,
        };
        let mut out = String::new();
        render_custom_queries(std::slice::from_ref(&q), &mut out);
        assert!(
            !out.contains("Note:"),
            "absent note must not emit a Note: line: {out}"
        );
    }

    #[test]
    fn str_cell_pipe_is_escaped() {
        let q = QueryResult {
            name: "pipes".into(),
            oql: "SELECT s FROM C".into(),
            columns: vec![col("s")],
            rows: vec![vec![QueryValue::Str("a|b".into())]],
            row_count: 1,
            truncated: false,
            error: None,
            note: None,
            viz: None,
            elapsed_ms: None,
        };
        let mut out = String::new();
        render_custom_queries(std::slice::from_ref(&q), &mut out);
        assert!(
            out.contains("| a\\|b |"),
            "pipe in Str cell must be escaped as \\|: {out}"
        );
    }

    #[test]
    fn multiple_queries_each_get_a_section() {
        let queries = vec![
            QueryResult {
                name: "first".into(),
                oql: "SELECT 1".into(),
                columns: vec![col("a")],
                rows: vec![vec![QueryValue::Int(1)]],
                row_count: 1,
                truncated: false,
                error: None,
                note: None,
                viz: None,
                elapsed_ms: None,
            },
            QueryResult {
                name: "second".into(),
                oql: "SELECT 2".into(),
                columns: vec![col("b")],
                rows: vec![vec![QueryValue::Int(2)]],
                row_count: 1,
                truncated: false,
                error: None,
                note: None,
                viz: None,
                elapsed_ms: None,
            },
        ];
        let mut out = String::new();
        render_custom_queries(&queries, &mut out);
        assert!(
            out.contains("### first"),
            "first query heading missing: {out}"
        );
        assert!(
            out.contains("### second"),
            "second query heading missing: {out}"
        );
        // Only one top-level section heading regardless of query count.
        assert_eq!(
            out.matches("## Custom Queries").count(),
            1,
            "exactly one section heading: {out}"
        );
    }

    #[test]
    fn zero_column_result_still_emits_a_separator() {
        // A pathological result with no columns should not produce a broken
        // separator row; the .max(1) keeps at least one `--- |` cell.
        let q = QueryResult {
            name: "nocols".into(),
            oql: "SELECT".into(),
            columns: vec![],
            rows: vec![],
            row_count: 0,
            truncated: false,
            error: None,
            note: None,
            viz: None,
            elapsed_ms: None,
        };
        let mut out = String::new();
        render_custom_queries(std::slice::from_ref(&q), &mut out);
        assert!(
            out.contains("| --- |"),
            "empty-column result must still emit a valid separator: {out}"
        );
    }

    #[test]
    fn fmt_query_value_covers_all_variants() {
        assert_eq!(fmt_query_value(&QueryValue::Null), "null");
        assert_eq!(fmt_query_value(&QueryValue::Bool(true)), "true");
        assert_eq!(fmt_query_value(&QueryValue::Bool(false)), "false");
        assert_eq!(fmt_query_value(&QueryValue::Int(-42)), "-42");
        assert_eq!(fmt_query_value(&QueryValue::Float(1.5)), "1.5");
        assert_eq!(fmt_query_value(&QueryValue::Str("hi".into())), "hi");
        assert_eq!(
            fmt_query_value(&QueryValue::ObjRef {
                index: 7,
                class: "java.lang.String".into(),
                addr: None,
            }),
            "java.lang.String@7"
        );
    }

    #[test]
    fn columns_but_zero_rows_emits_header_separator_and_footer_only() {
        // A query that matched nothing: header + separator + empty body + footer.
        let q = QueryResult {
            name: "empty".into(),
            oql: "SELECT name FROM C WHERE 1 = 0".into(),
            columns: vec![col("name")],
            rows: vec![],
            row_count: 0,
            truncated: false,
            error: None,
            note: None,
            viz: None,
            elapsed_ms: None,
        };
        let mut out = String::new();
        render_custom_queries(std::slice::from_ref(&q), &mut out);
        assert!(out.contains("| name |"), "header row missing: {out}");
        assert!(out.contains("| --- |"), "separator row missing: {out}");
        assert!(out.contains("_0 row(s)_"), "zero-row footer missing: {out}");
        // No stray body row between the separator and the footer.
        assert!(
            !out.contains("| null |"),
            "must not emit a body row for a rowless result: {out}"
        );
    }

    #[test]
    fn float_and_objref_render_through_the_table_path() {
        // Exercise Float and ObjRef via the full table renderer, not just the
        // fmt_query_value unit — the cells must appear formatted in a data row.
        let q = QueryResult {
            name: "mixed".into(),
            oql: "SELECT @usedHeapSize, this FROM C".into(),
            columns: vec![col("size"), col("obj")],
            rows: vec![vec![
                QueryValue::Float(2.5),
                QueryValue::ObjRef {
                    index: 12,
                    class: "java.lang.String".into(),
                    addr: None,
                },
            ]],
            row_count: 1,
            truncated: false,
            error: None,
            note: None,
            viz: None,
            elapsed_ms: None,
        };
        let mut out = String::new();
        render_custom_queries(std::slice::from_ref(&q), &mut out);
        assert!(
            out.contains("| 2.5 | java.lang.String@12 |"),
            "Float and ObjRef cells must render in the data row: {out}"
        );
    }

    fn viz(kind: crate::query::viz::VizKind) -> crate::query::viz::VizSpec {
        crate::query::viz::VizSpec {
            kind,
            label_col: Some("name".into()),
            value_col: Some("bytes".into()),
            cap: None,
            ..Default::default()
        }
    }

    fn charted_result(kind: crate::query::viz::VizKind) -> QueryResult {
        QueryResult {
            name: "chart".into(),
            oql: "SELECT @displayName AS name, @usedHeapSize AS bytes FROM C".into(),
            columns: vec![col("name"), col("bytes")],
            rows: vec![
                vec![QueryValue::Str("alpha".into()), QueryValue::Int(10)],
                vec![QueryValue::Str("beta".into()), QueryValue::Int(30)],
            ],
            row_count: 2,
            truncated: false,
            error: None,
            note: None,
            viz: Some(viz(kind)),
            elapsed_ms: None,
        }
    }

    #[test]
    fn histogram_viz_renders_ascii_bars() {
        let q = charted_result(crate::query::viz::VizKind::Histogram);
        let mut out = String::new();
        render_custom_queries(std::slice::from_ref(&q), &mut out);
        // The table still shows all rows.
        assert!(
            out.contains("| name | bytes |"),
            "table header missing: {out}"
        );
        // A fenced ASCII bar block follows, with labels and `#` bars.
        assert!(out.contains("alpha"), "bar label alpha missing: {out}");
        assert!(out.contains("beta"), "bar label beta missing: {out}");
        assert!(out.contains('#'), "ascii bar chars missing: {out}");
        // The larger value (30) yields more `#` than the smaller (10). Scan only
        // the bar lines (those containing `#`), not the plain markdown table rows.
        let alpha_hashes = out
            .lines()
            .find(|l| l.contains("alpha") && l.contains('#'))
            .map(|l| l.matches('#').count())
            .unwrap_or(0);
        let beta_hashes = out
            .lines()
            .find(|l| l.contains("beta") && l.contains('#'))
            .map(|l| l.matches('#').count())
            .unwrap_or(0);
        assert!(
            beta_hashes > alpha_hashes,
            "larger value must draw a longer bar: alpha={alpha_hashes} beta={beta_hashes}\n{out}"
        );
    }

    #[test]
    fn viz_title_renders_as_heading_above_chart() {
        let mut spec = viz(crate::query::viz::VizKind::Histogram);
        spec.title = Some("Top classes by size".into());
        let mut q = charted_result(crate::query::viz::VizKind::Histogram);
        q.viz = Some(spec);
        let mut out = String::new();
        render_custom_queries(std::slice::from_ref(&q), &mut out);
        assert!(
            out.contains("**Top classes by size**"),
            "title heading missing: {out}"
        );
        let title_pos = out.find("**Top classes by size**").unwrap();
        // The chart fence is the one AFTER the title (the first ``` is the OQL block).
        let chart_fence_pos = out[title_pos..].find("```").map(|p| title_pos + p).unwrap();
        assert!(
            title_pos < chart_fence_pos,
            "title must precede the chart: {out}"
        );
    }

    #[test]
    fn piechart_viz_shows_percent_share() {
        let q = charted_result(crate::query::viz::VizKind::Piechart);
        let mut out = String::new();
        render_custom_queries(std::slice::from_ref(&q), &mut out);
        // 10 and 30 of a 40 total -> 25.0% and 75.0%.
        assert!(out.contains("25.0%"), "alpha share missing: {out}");
        assert!(out.contains("75.0%"), "beta share missing: {out}");
    }

    #[test]
    fn treemap_viz_notes_html_only() {
        let q = charted_result(crate::query::viz::VizKind::Treemap);
        let mut out = String::new();
        render_custom_queries(std::slice::from_ref(&q), &mut out);
        assert!(
            out.contains("Treemap chart is available in the HTML report"),
            "treemap must explain HTML-only rendering: {out}"
        );
        // No ascii bar block for a treemap: no bar line (label + `| #…`) exists.
        // (Markdown headers like `## Custom Queries` also contain `#`, so match
        // the bar shape specifically rather than a bare `#`.)
        assert!(
            !out.lines().any(|l| l.contains("| #")),
            "treemap must not draw ascii bars: {out}"
        );
    }

    #[test]
    fn table_viz_and_no_viz_draw_no_chart() {
        for spec in [Some(viz(crate::query::viz::VizKind::Table)), None] {
            let mut q = charted_result(crate::query::viz::VizKind::Histogram);
            q.viz = spec;
            let mut out = String::new();
            render_custom_queries(std::slice::from_ref(&q), &mut out);
            // The table renders, but no fenced bar block beyond the OQL block.
            // (histogram would add a second ``` block; table/None must not.)
            let fences = out.matches("```").count();
            assert_eq!(
                fences, 2,
                "only the OQL fence should appear (2 backtick markers), got {fences}:\n{out}"
            );
        }
    }

    #[test]
    fn viz_cap_limits_charted_rows_not_table() {
        let mut q = charted_result(crate::query::viz::VizKind::Histogram);
        // Add a third row and cap the chart at 2.
        q.rows
            .push(vec![QueryValue::Str("gamma".into()), QueryValue::Int(5)]);
        q.row_count = 3;
        if let Some(v) = q.viz.as_mut() {
            v.cap = Some(2);
        }
        let mut out = String::new();
        render_custom_queries(std::slice::from_ref(&q), &mut out);
        // The table shows all three rows.
        assert!(
            out.contains("gamma"),
            "table must show all rows incl gamma: {out}"
        );
        // The chart block (after the OQL fence) charts only the first two.
        let chart_block = out.rsplit("```").nth(1).unwrap_or("");
        assert!(
            !chart_block.contains("gamma"),
            "capped chart must omit gamma: {chart_block}"
        );
    }
}

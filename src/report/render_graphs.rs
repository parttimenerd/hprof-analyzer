//! ASCII-graph (`--md-graphs`) renderers: mirror the plain sections with
//! proportional bars, sparklines, and tree-drawn hierarchies.

use super::*;

/// `md-graphs` output: the same Markdown report enriched with in-text ASCII/Unicode
/// graphics (a linked table of contents, proportional bar columns, tree-drawn
/// packages, and a sparkline depth histogram). Rendered independently of plain
/// `md` so the byte-exact `md` output is never perturbed.
pub fn render_markdown_graphs(r: &Report) -> String {
    let mut out = String::new();
    render_title(&r.overview, &r.generated, &mut out);
    render_toc_graphs(r, &mut out);
    render_executive_summary(r, &mut out);
    render_oom_triage(r, &mut out);
    render_waste_summary_graphs(r, &mut out);
    render_system_overview_graphs(&r.overview, &mut out);
    render_leak_suspects_graphs(&r.leaks, &mut out);
    render_top_consumers_graphs(&r.top, r.leaks.total_shallow, &mut out);
    render_dominator_analysis(&r.dominator_analysis, true, &mut out);
    render_threads(&r.threads, true, &mut out);
    render_top_components(&r.top_components, true, &mut out);
    render_arrays_by_size(&r.arrays_by_size, true, &mut out);
    render_collections(&r.collections, &r.collection_attribution, true, &mut out);
    render_collection_attribution(&r.collection_attribution, true, &mut out);
    render_fields_by_size(&r.fields_by_size, true, &mut out);
    render_biggest_collections(&r.biggest_collections, true, &mut out);
    render_collection_contents(&r.collection_contents, true, &mut out);
    render_references(&r.references, true, &mut out);
    render_unreachable_histogram(&r.overview, true, &mut out);
    // Allocation sites (always present; `None` only for legacy reports).
    if let Some(a) = &r.alloc_sites {
        render_alloc_sites(a, true, &mut out);
    }
    render_retention_concentration_graphs(&r.overview, &mut out);
    render_dominator_depth_graphs(&r.overview, &mut out);
    render_leak_indicators(&r.leak_indicators, &mut out);
    render_glossary(&mut out);
    out
}

/// Waste Summary with a proportional bar per source (mirrors the plain-md
/// section but adds a `bar` column scaled to the largest source).
fn render_waste_summary_graphs(r: &Report, out: &mut String) {
    use crate::md::{Align, Table, bar};
    let Some(w) = r.waste_summary.as_ref() else {
        return;
    };
    if w.total_bytes == 0 {
        return;
    }
    out.push_str("## Waste Summary\n\n");
    out.push_str(&format!(
        "_Approximately **{}** looks reclaimable across the sources below. Figures are approximate and may overlap slightly._\n\n",
        format_bytes(w.total_bytes)
    ));
    let max = w.sources.iter().map(|s| s.bytes).max().unwrap_or(0);
    let mut t = Table::new(
        &["Source", "Reclaimable", ""],
        &[Align::Left, Align::Right, Align::Left],
    );
    for s in &w.sources {
        let label = match &s.anchor {
            Some(a) => format!("[{}](#{})", s.label, a),
            None => s.label.clone(),
        };
        t.row([label, format_bytes(s.bytes), bar(s.bytes, max, 16)]);
    }
    t.render(out);
    out.push('\n');
}


/// use GitHub's slug convention (lowercase, spaces → hyphens) matching the
/// `##`/`###` headings emitted by the section renderers.
fn render_toc_graphs(r: &Report, out: &mut String) {
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
    if r.fields_by_size.as_ref().map_or(false, |f| !f.rows.is_empty()) {
        out.push_str(&SectionId::FieldsBySize.toc_bullet());
    }
    if r.biggest_collections.as_ref().map_or(false, |b| {
        !b.combined.is_empty() || !b.by_kind.is_empty()
    }) {
        out.push_str(&SectionId::BiggestCollections.toc_bullet());
    }
    if r.collection_contents.as_ref().map_or(false, |c| !c.rows.is_empty()) {
        out.push_str(&SectionId::CollectionContents.toc_bullet());
    }
    out.push_str(&SectionId::References.toc_bullet());
    out.push_str(&SectionId::UnreachableObjects.toc_bullet());
    // The ToC bullet appears only when the alloc-sites section is present.
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

// ── md-graphs section renderers ─────────────────────────────────────────────
// These mirror the plain-Markdown sections byte-for-byte in their data, but add
// proportional bar columns, a sparkline, and tree-drawn package hierarchy. They
// are only reachable from `render_markdown_graphs`; plain `md` never calls them.

/// Width (in cells) of the in-table proportional bar columns. Fixed so columns
/// stay aligned regardless of the values.
pub(crate) const GRAPH_BAR_WIDTH: usize = 16;

/// System Overview with bar columns on GC Roots / Heap Composition, a sparkline
/// for the dominator-depth distribution, and a share bar on the class histogram.
fn render_system_overview_graphs(o: &SystemOverview, out: &mut String) {
    use crate::md::{Align, Table, bar};
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
    summary.row([HEAP_SCALAR_LABEL.into(), format_bytes(o.total_shallow)]);
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

    // GC Roots by Type — with a proportional count bar.
    if o.gc_roots_by_type.len() > 1 {
        out.push_str("### GC Roots by Type\n\n");
        let max = o
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
                bar(row.count, max, GRAPH_BAR_WIDTH),
            ]);
        }
        t.render(out);
        out.push('\n');
    }

    // Heap Composition — with a proportional shallow-heap bar scaled to total heap.
    if o.heap_composition.by_kind.len() > 1 {
        out.push_str("### Heap Composition\n\n");
        let mut t = Table::new(
            &["Kind", "Objects", "Shallow Heap", ""],
            &[Align::Left, Align::Right, Align::Right, Align::Left],
        );
        for k in &o.heap_composition.by_kind {
            t.row([
                k.kind.clone(),
                fmt_count(k.objects),
                format_bytes(k.shallow_heap),
                bar(k.shallow_heap, o.total_shallow, GRAPH_BAR_WIDTH),
            ]);
        }
        t.render(out);
        out.push('\n');
    }

    render_record_census(out, &o.record_census);
    render_duplicate_strings(out, &o.duplicate_strings, true);
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
    let hist_max = o.total_shallow;
    let mut hist = Table::new(
        &[
            "#",
            "Class",
            "Instances",
            "Shallow Heap",
            "Largest",
            "Retained Heap",
            "% Heap",
            "",
        ],
        &[
            Align::Right,
            Align::Left,
            Align::Right,
            Align::Right,
            Align::Right,
            Align::Right,
            Align::Right,
            Align::Left,
        ],
    );
    for (rank, row) in o.histogram.iter().take(50).enumerate() {
        hist.row([
            (rank + 1).to_string(),
            format!("`{}`", row.pretty_class),
            fmt_count(row.instances),
            format_bytes(row.shallow),
            format_bytes(row.max_instance_shallow),
            format_bytes(row.retained),
            fmt_pct(pct_of_heap(row.retained, o.total_shallow)),
            bar(row.retained, hist_max, GRAPH_BAR_WIDTH),
        ]);
    }
    hist.render(out);
    out.push('\n');

    // Class Loaders (F2) — with a proportional retained-heap bar.
    if !o.loader_rollup.is_empty() {
        out.push_str("### Class Loaders\n\n");
        out.push_str(
            "_Classes grouped by the loader that defined them; many loaders each holding heap \
             can signal a class-loader leak._\n\n",
        );
        let lmax = o.total_shallow;
        let mut t = Table::new(
            &[
                "Loader",
                "Classes",
                "Instances",
                "Shallow Heap",
                "Retained Heap",
                "",
            ],
            &[
                Align::Left,
                Align::Right,
                Align::Right,
                Align::Right,
                Align::Right,
                Align::Left,
            ],
        );
        for r in &o.loader_rollup {
            t.row([
                r.loader_label.clone().unwrap_or_else(|| "<unknown>".into()),
                fmt_count(r.class_count),
                fmt_count(r.instances),
                format_bytes(r.shallow),
                format_bytes(r.retained),
                bar(r.retained, lmax, GRAPH_BAR_WIDTH),
            ]);
        }
        t.render(out);
        out.push('\n');
    }

    // Duplicate Classes (F2) — same table as plain md (no extra glyph column;
    // #Loaders is already the salient number).
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
        // In graphs mode, a proportional bar on Retained highlights the leader.
        for d in &o.duplicate_classes {
            if d.per_loader.is_empty() {
                continue;
            }
            out.push_str(&format!("**`{}`** — per loader:\n\n", d.pretty_class));
            let rmax = o.total_shallow;
            // Disambiguate loaders that share a display label by appending id.
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
            let mut lt = Table::new(
                &["Loader", "Instances", "Shallow", "Retained Heap", ""],
                &[
                    Align::Left,
                    Align::Right,
                    Align::Right,
                    Align::Right,
                    Align::Left,
                ],
            );
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
                    bar(pl.retained, rmax, GRAPH_BAR_WIDTH),
                ]);
            }
            lt.render(out);
            out.push('\n');
        }
    }
}

/// Retention Concentration (md-graphs): same numbers as plain md plus a
/// proportional bar column. Standalone section near the end of the report.
fn render_retention_concentration_graphs(o: &SystemOverview, out: &mut String) {
    use crate::md::{Align, Table, bar};
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
    let mut t = Table::new(
        &["Scope", "Retained Share", "Retained", ""],
        &[Align::Left, Align::Right, Align::Right, Align::Left],
    );
    t.row([
        "Top 1 object".into(),
        fmt_pct(rc.top1_bp as f64 / 100.0),
        format_bytes(rc.top1_retained),
        bar(rc.top1_bp as u64, 10_000, GRAPH_BAR_WIDTH),
    ]);
    t.row([
        "Top 10 objects".into(),
        fmt_pct(rc.top10_bp as f64 / 100.0),
        format_bytes(rc.top10_retained),
        bar(rc.top10_bp as u64, 10_000, GRAPH_BAR_WIDTH),
    ]);
    t.row([
        "Top 100 objects".into(),
        fmt_pct(rc.top100_bp as f64 / 100.0),
        format_bytes(rc.top100_retained),
        bar(rc.top100_bp as u64, 10_000, GRAPH_BAR_WIDTH),
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

/// Dominator-Depth Distribution (md-graphs): the full per-depth table with a
/// proportional bar column. Delegates to the shared render_md implementation.
fn render_dominator_depth_graphs(o: &SystemOverview, out: &mut String) {
    render_md::render_dominator_depth_graphs(o, out);
}

/// Leak Suspects with a leading share-bar table across all suspects, then the
/// full plain per-suspect detail (reused verbatim for byte-identical numbers).
fn render_leak_suspects_graphs(l: &LeakSuspects, out: &mut String) {
    use crate::md::{Align, Table, bar};
    out.push_str("## Leak Suspects\n\n");

    if l.suspects.is_empty() {
        out.push_str("No single object or class group exceeds the threshold.\n\n");
        return;
    }

    out.push_str(
        "_Objects and class groups retaining the most heap, ranked by retained size. \
These are the most likely accumulation points for excessive memory usage._\n\n",
    );

    // Share overview: one proportional bar per suspect, keyed to the largest
    // suspect's retained heap so the relative sizes read at a glance.
    let mut share = Table::new(
        &["#", "Suspect", "Retained", "% Heap", ""],
        &[
            Align::Right,
            Align::Left,
            Align::Right,
            Align::Right,
            Align::Left,
        ],
    );
    for (rank, s) in l.suspects.iter().enumerate() {
        let pct = pct_of_heap(s.retained, l.total_shallow);
        share.row([
            (rank + 1).to_string(),
            format!("`{}`", s.pretty_class),
            format_bytes(s.retained),
            fmt_pct(pct),
            bar(s.retained, l.total_shallow, GRAPH_BAR_WIDTH),
        ]);
    }
    share.render(out);
    out.push('\n');

    // Per-suspect detail: identical to plain Markdown.
    for (rank, s) in l.suspects.iter().enumerate() {
        let pct = pct_of_heap(s.retained, l.total_shallow);

        out.push_str(&format!(
            "### {}. `{}` — retains {} ({} of {HEAP_BASIS_LABEL})\n\n",
            rank + 1,
            s.pretty_class,
            format_bytes(s.retained),
            fmt_pct(pct),
        ));

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

        if !s.dominated.is_empty() {
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

        if !s.dominated_by_class.is_empty() {
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

        // Dominator chain to a GC root: identical numbered list as plain md.
        if let Some(path) = &s.root_path {
            render_root_path(path, out);
        }
        // Full dominator subtree: box-drawn tree in the graphs report.
        if let Some(tree) = &s.dominator_tree {
            render_dom_tree_graphs(tree, out);
        }
        // Merged shortest paths to GC roots (group suspects only): box-drawn tree.
        if !s.is_single {
            if let Some(root) = &s.merged_paths {
                render_merged_paths_graphs(root, out);
            }
        }
    }
}

/// Top Consumers with share bars on Biggest Objects / Classes and a tree-drawn
/// package hierarchy (box-drawing connectors + a retained-heap bar per row).
fn render_top_consumers_graphs(t: &TopConsumers, total_shallow: u64, out: &mut String) {
    use crate::md::{Align, Table, bar, tree_prefix};
    out.push_str("## Top Consumers\n\n");
    out.push_str("### Biggest Objects (Top-Level Dominators)\n\n");
    out.push_str(
        "_Individual objects retaining the most heap; `% Heap` is the share of total reachable heap._\n\n",
    );
    // "Held via" names the dominant incoming `Class#field` referrer; present
    // only when attribution data exists (`--collections`).
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
    obj_headers.push("");
    obj_aligns.push(Align::Left);
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
        cells.push(bar(row.retained, total_shallow, GRAPH_BAR_WIDTH));
        objs.row(cells);
    }
    objs.render(out);
    out.push('\n');

    out.push_str("### Biggest Classes by Retained Heap\n\n");
    out.push_str("_Classes whose instances together retain the most heap._\n\n");
    let cls_max = total_shallow;
    let mut classes = Table::new(
        &["#", "Class", "Instances", "Retained Heap", ""],
        &[
            Align::Right,
            Align::Left,
            Align::Right,
            Align::Right,
            Align::Left,
        ],
    );
    for (rank, row) in t.biggest_classes.iter().enumerate() {
        classes.row([
            (rank + 1).to_string(),
            format!("`{}`", row.pretty_class),
            fmt_count(row.instances),
            format_bytes(row.retained),
            bar(row.retained, cls_max, GRAPH_BAR_WIDTH),
        ]);
    }
    classes.render(out);
    out.push('\n');

    // Top-Dominator Size Distribution — sparkline over bucket counts plus a
    // bar()-column bucket table (mirrors the dominator-depth graphs render).
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
        let counts: Vec<u64> = d.buckets.iter().map(|b| b.count).collect();
        let bmax = counts.iter().copied().max().unwrap_or(0);
        let mut buckets = Table::new(
            &["Size ≤", "Count", "% of Dom.", ""],
            &[Align::Right, Align::Right, Align::Right, Align::Left],
        );
        for b in &d.buckets {
            let pct = if d.count > 0 {
                fmt_pct(b.count as f64 / d.count as f64 * 100.0)
            } else {
                "—".into()
            };
            buckets.row([
                format_bytes(b.upper_bytes),
                fmt_count(b.count),
                pct,
                bar(b.count, bmax, GRAPH_BAR_WIDTH),
            ]);
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
        "_Retained heap aggregated by package prefix (rows retaining <1% of the total are pruned); the tree shows nesting._\n\n",
    );
    // Bar is keyed to total reachable heap for cross-section comparability.
    let pkg_max = total_shallow;
    let mut pkgs = Table::new(
        &["Package", "Objects", "Shallow", "Retained", ""],
        &[
            Align::Left,
            Align::Right,
            Align::Right,
            Align::Right,
            Align::Left,
        ],
    );
    // Pre-order DFS with box-drawing prefixes. Each row shows only this node's
    // own segment name (last dotted component), indented by its tree position;
    // the full path is implied by the nesting rather than repeated.
    fn emit_node_tree(
        node: &PackageNode,
        depth: usize,
        is_last: bool,
        ancestors_continue: &[bool],
        pkg_max: u64,
        pkgs: &mut Table,
    ) {
        let prefix = tree_prefix(depth, is_last, ancestors_continue);
        // Show the leaf segment (final dotted component) for depth > 0; the full
        // name at the top level so top rows stay self-describing.
        let label = if depth == 0 {
            node.name.clone()
        } else {
            node.name
                .rsplit('.')
                .next()
                .unwrap_or(&node.name)
                .to_string()
        };
        pkgs.row([
            format!("{prefix}`{label}`"),
            fmt_count(node.top_dominator_count),
            format_bytes(node.shallow_heap),
            format_bytes(node.retained_heap),
            bar(node.retained_heap, pkg_max, GRAPH_BAR_WIDTH),
        ]);
        let n = node.children.len();
        for (i, child) in node.children.iter().enumerate() {
            let child_last = i + 1 == n;
            let mut cont = ancestors_continue.to_vec();
            cont.push(!is_last);
            emit_node_tree(child, depth + 1, child_last, &cont, pkg_max, pkgs);
        }
    }
    let n = t.biggest_packages.children.len();
    for (i, child) in t.biggest_packages.children.iter().enumerate() {
        emit_node_tree(child, 0, i + 1 == n, &[], pkg_max, &mut pkgs);
    }
    pkgs.render(out);
    out.push('\n');
}

/// Dominator subtree (md-graphs): the same subtree drawn with
/// box-drawing connectors via `md::tree_prefix`. Explicit-stack pre-order walk
/// tracking `is_last` + the per-ancestor "continue" flags the prefix needs.
fn render_dom_tree_graphs(root: &DomTreeNode, out: &mut String) {
    use crate::md::tree_prefix;
    out.push_str("**Dominator subtree:**\n\n");
    // Each stack frame carries the node, its depth, whether it is the last
    // child at its level, and the ancestors-continue flags for its level.
    struct Frame<'a> {
        node: &'a DomTreeNode,
        depth: usize,
        is_last: bool,
        ancestors_continue: Vec<bool>,
    }
    let mut stack: Vec<Frame> = vec![Frame {
        node: root,
        depth: 0,
        is_last: true,
        ancestors_continue: Vec::new(),
    }];
    out.push_str("```\n");
    while let Some(f) = stack.pop() {
        let prefix = tree_prefix(f.depth, f.is_last, &f.ancestors_continue);
        out.push_str(&format!(
            "{}{} (shallow {}, retained {})\n",
            prefix,
            f.node.display_class,
            format_bytes(f.node.shallow),
            format_bytes(f.node.retained),
        ));
        // Push children reversed so the pre-order left-to-right order is kept.
        let n = f.node.children.len();
        for (i, child) in f.node.children.iter().enumerate().rev() {
            let mut cont = f.ancestors_continue.clone();
            cont.push(!f.is_last);
            stack.push(Frame {
                node: child,
                depth: f.depth + 1,
                is_last: i + 1 == n,
                ancestors_continue: cont,
            });
        }
    }
    out.push_str("```\n\n");
}

/// Merged shortest paths to GC roots (graphs md): the member objects' dominator
/// chains collapsed into a class-keyed prefix tree, drawn as a box-drawing tree
/// — mirroring `render_dom_tree_graphs` so the visual language is consistent.
fn render_merged_paths_graphs(root: &MergedPathNode, out: &mut String) {
    use crate::md::tree_prefix;
    out.push_str("#### Merged Paths to GC Roots\n\n");
    struct Frame<'a> {
        node: &'a MergedPathNode,
        depth: usize,
        is_last: bool,
        ancestors_continue: Vec<bool>,
    }
    let mut stack: Vec<Frame> = vec![Frame {
        node: root,
        depth: 0,
        is_last: true,
        ancestors_continue: Vec::new(),
    }];
    out.push_str("```\n");
    while let Some(f) = stack.pop() {
        let prefix = tree_prefix(f.depth, f.is_last, &f.ancestors_continue);
        let mut line = format!(
            "{}{} ({} objects, retained {})",
            prefix,
            f.node.display_class,
            fmt_count(f.node.object_count),
            format_bytes(f.node.retained),
        );
        if let Some(label) = &f.node.root_type_label {
            line.push_str(&format!(" — GC root: {label}"));
        }
        line.push('\n');
        out.push_str(&line);
        let n = f.node.children.len();
        for (i, child) in f.node.children.iter().enumerate().rev() {
            let mut cont = f.ancestors_continue.clone();
            cont.push(!f.is_last);
            stack.push(Frame {
                node: child,
                depth: f.depth + 1,
                is_last: i + 1 == n,
                ancestors_continue: cont,
            });
        }
    }
    out.push_str("```\n\n");
}

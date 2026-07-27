//! Library interface for hprof-analyzer, used by the hprof-wasm WASM crate.

mod bitset;
mod chunkvec;
mod collection_config;
pub mod cvec;
mod dominator;
mod id_map;
mod md;
#[cfg(test)]
mod md_test;
pub mod named_queries;
pub mod opts;
mod pass1;
pub use pass1::Pass1;
mod pass2;
mod progress;
pub mod query;
mod reader;
pub mod source;
pub use source::HprofSource;
pub mod report;
mod retained;
mod rpo_dfs;
pub mod run_oql;
mod sweep;
mod trace;
pub mod types;
mod unreachable_retained;
mod vbyte;

pub use opts::{AnalyzeOptions, DetailLevel, OutputFormat};

/// Run full analysis returning (Report, per-object retained-size vec).
pub fn analyze_to_report_with_retained(
    path: &str,
    opts: &AnalyzeOptions,
) -> std::io::Result<(crate::report::Report, Vec<u64>)> {
    analyze_to_report_inner(path, opts)
}

fn analyze_to_report_inner(
    path: &str,
    opts: &AnalyzeOptions,
) -> std::io::Result<(crate::report::Report, Vec<u64>)> {
    use std::io;

    let source = crate::source::HprofSource::from(path);
    let p1 = pass1::Pass1::run(&source, false)?;

    if p1.class_ids.len() > u32::MAX as usize {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "dump has {} objects, exceeding the {} (u32::MAX) limit of the \
                 analyzer's index scheme; cannot analyze",
                p1.class_ids.len(),
                u32::MAX
            ),
        ));
    }

    let compress = cvec::Codec::Deflate9;

    let mut no_in_sets = std::collections::HashMap::new();
    let mut no_exists_bools = std::collections::HashMap::new();
    let (
        mut g,
        mut inbound,
        shallow_c,
        class_idx_c,
        alloc_serial_c,
        _query_state,
        _refwalk_csr,
        _string_values,
        _string_values_truncated,
    ) = pass2::Pass2::build(&source, p1, compress, opts, &[], &mut no_in_sets, &mut no_exists_bools)?;

    inbound.compress_id_map(compress)?;

    let rpo = rpo_dfs::rpo_dfs(g.n, &g.gc_root_indices, &g.fwd_offsets, &g.fwd_targets);

    {
        g.unreachable_retained = unreachable_retained::compute_unreachable_retained(
            g.n,
            &rpo.dfn,
            &g.fwd_offsets,
            &g.fwd_targets,
            &shallow_c,
            &class_idx_c,
            g.class_names.len(),
            &g.class_obj_class_idx,
            &g.class_names,
        )?;
    }

    let mut rpo = rpo;
    let parent_pre_count = rpo.parent_pre.len();
    let parent_pre_c = if compress != cvec::Codec::None {
        let c = cvec::CompressedU32::compress(&rpo.parent_pre, compress)?;
        rpo.parent_pre = Vec::new();
        Some(c)
    } else {
        None
    };

    // build_from_fwd needs dfn alive; it is cleared afterward (matching run()).
    let (inb_block_off, inb_data) = inbound.build_from_fwd(
        std::mem::take(&mut g.fwd_offsets),
        std::mem::take(&mut g.fwd_targets),
        &rpo.dfn,
    )?;

    // Rebuild vertex while dfn is still live; then free dfn.
    let count = parent_pre_count;
    rpo.vertex = rpo_dfs::rebuild_vertex(&rpo.dfn, count);
    rpo.dfn = Vec::new();

    if let Some(c) = parent_pre_c {
        rpo.parent_pre = c.restore()?;
    }

    g.idom =
        dominator::compute_dominators(g.n, rpo, &g.gc_root_indices, &inb_block_off, &inb_data)?;
    drop(inb_block_off);
    drop(inb_data);

    let (dc_off, dc_tgt) = retained::build_dom_children_csr(g.n, &g.idom);

    if compress != cvec::Codec::None {
        g.shallow = shallow_c.restore()?;
        g.class_idx = class_idx_c.restore()?;
    }
    drop(shallow_c);
    drop(class_idx_c);

    let class_count = g.class_names.len();
    let (retained, has_same, depth_counts) = retained::compute_retained(
        g.n,
        &g.idom,
        &g.shallow,
        &g.class_idx,
        class_count,
        &g.class_obj_class_idx,
        &dc_off,
        &dc_tgt,
    );
    g.retained = retained;
    g.has_same_class_ancestor = has_same;

    let alloc_sites = if let Some(c) = alloc_serial_c {
        let mut agg = report::AllocAgg::new(&g, opts.alloc_sites_top);
        c.for_each_u32(|serial| agg.push(serial))?;
        let a = agg.finish();
        g.alloc_frames_by_serial = None;
        Some(a)
    } else {
        let a = report::build_alloc_sites(&g, opts.alloc_sites_top);
        g.alloc_stack_serial = Vec::new();
        g.alloc_frames_by_serial = None;
        Some(a)
    };

    let report = report::build_model(
        &g,
        &dc_off,
        &dc_tgt,
        opts.leak_children_cap,
        &depth_counts,
        opts,
        alloc_sites,
    );
    drop(dc_off);
    drop(dc_tgt);

    // Extract the per-object retained-size array before g is dropped.
    // The caller (analyze_to_report_with_retained) stores this for OQL reuse.
    let retained = std::mem::take(&mut g.retained);

    Ok((report, retained))
}

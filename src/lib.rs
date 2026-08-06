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

/// Render a pre-serialized report JSON into a self-contained HTML document.
///
/// The HTML is identical to `hprof-analyzer analyze --format html` output:
/// the same React bundle and bootstrap are embedded. Used by the WASM crate's
/// `generate_report_html()` and by the CLI's `html.rs::render_html`.
pub fn render_report_html(source_name: &str, report_json: &str) -> String {
    use base64::Engine as _;
    use std::io::Write as _;

    static BUNDLE_DEFLATED: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/bundle.deflate"));

    fn b64(bytes: &[u8]) -> String {
        base64::engine::general_purpose::STANDARD.encode(bytes)
    }

    fn deflate_b64(bytes: &[u8]) -> String {
        let mut enc = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::new(9));
        enc.write_all(bytes).expect("deflate write");
        b64(&enc.finish().expect("deflate finish"))
    }

    let data_b64 = deflate_b64(report_json.as_bytes());
    let bundle_b64 = b64(BUNDLE_DEFLATED);

    let title = source_name
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");

    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Heap Dump Analysis: {title}</title>
<style>
:root {{ color-scheme: light dark; }}
html, body {{ margin: 0; padding: 0; font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, Helvetica, Arial, sans-serif; }}
#root {{ padding: 0; }}
#hprof-fallback {{ padding: 1rem; }}
</style>
</head>
<body>
<div id="root"><div id="hprof-fallback">Loading heap dump report&hellip;</div></div>
<script type="application/octet-stream" id="report-data">{data_b64}</script>
<script type="application/octet-stream" id="app-bundle">{bundle_b64}</script>
<script>{BOOTSTRAP_JS}</script>
</body>
</html>
"#
    )
}

const BOOTSTRAP_JS: &str = r#"(function(){function b64ToBytes(b64){var bin=atob(b64);var len=bin.length;var out=new Uint8Array(len);for(var i=0;i<len;i++)out[i]=bin.charCodeAt(i);return out;}async function inflate(b64){var bytes=b64ToBytes(b64);if(typeof DecompressionStream==="function"){var ds=new DecompressionStream("deflate-raw");var stream=new Response(new Blob([bytes]).stream().pipeThrough(ds));var buf=await stream.arrayBuffer();return new Uint8Array(buf);}return tinfl(bytes);}function tinfl(input){var out=[],op=0,ip=0,bitBuf=0,bitCnt=0;function need(n){while(bitCnt<n){bitBuf|=input[ip++]<<bitCnt;bitCnt+=8;}}function bits(n){need(n);var v=bitBuf&((1<<n)-1);bitBuf>>=n;bitCnt-=n;return v;}function build(lens){var max=0;for(var i=0;i<lens.length;i++)if(lens[i]>max)max=lens[i];var cnt=new Array(max+1).fill(0);for(i=0;i<lens.length;i++)cnt[lens[i]]++;cnt[0]=0;var next=new Array(max+1).fill(0),code=0;for(i=1;i<=max;i++){code=(code+cnt[i-1])<<1;next[i]=code;}var codes={};for(i=0;i<lens.length;i++){var l=lens[i];if(l){codes[l+"_"+next[l]]=i;next[l]++;}}return{codes:codes,max:max};}function decode(t){var code=0;for(var l=1;l<=t.max;l++){code=(code<<1)|bits(1);var s=t.codes[l+"_"+code];if(s!==undefined)return s;}throw"bad code";}var LB=[3,4,5,6,7,8,9,10,11,13,15,17,19,23,27,31,35,43,51,59,67,83,99,115,131,163,195,227,258];var LE=[0,0,0,0,0,0,0,0,1,1,1,1,2,2,2,2,3,3,3,3,4,4,4,4,5,5,5,5,0];var DB=[1,2,3,4,5,7,9,13,17,25,33,49,65,97,129,193,257,385,513,769,1025,1537,2049,3073,4097,6145,8193,12289,16385,24577];var DE=[0,0,0,0,1,1,2,2,3,3,4,4,5,5,6,6,7,7,8,8,9,9,10,10,11,11,12,12,13,13];var CLO=[16,17,18,0,8,7,9,6,10,5,11,4,12,3,13,2,14,1,15];while(true){var last=bits(1),type=bits(2);if(type===0){bitBuf=0;bitCnt=0;var lenv=input[ip]|(input[ip+1]<<8);ip+=4;for(var k=0;k<lenv;k++)out[op++]=input[ip++];}else{var lt,dt;if(type===1){var ll=[];for(var i=0;i<288;i++)ll.push(i<144?8:i<256?9:i<280?7:8);var dl=[];for(i=0;i<30;i++)dl.push(5);lt=build(ll);dt=build(dl);}else{var hlit=bits(5)+257,hdist=bits(5)+1,hclen=bits(4)+4;var cl=new Array(19).fill(0);for(i=0;i<hclen;i++)cl[CLO[i]]=bits(3);var ct=build(cl);var all=[];while(all.length<hlit+hdist){var s2=decode(ct);if(s2<16)all.push(s2);else if(s2===16){var r=bits(2)+3,p=all[all.length-1];while(r--)all.push(p);}else if(s2===17){var r2=bits(3)+3;while(r2--)all.push(0);}else{var r3=bits(7)+11;while(r3--)all.push(0);}}lt=build(all.slice(0,hlit));dt=build(all.slice(hlit));}while(true){var sym=decode(lt);if(sym===256)break;if(sym<256){out[op++]=sym;}else{sym-=257;var length=LB[sym]+bits(LE[sym]);var ds2=decode(dt);var dist=DB[ds2]+bits(DE[ds2]);for(var c=0;c<length;c++){out[op]=out[op-dist];op++;}}}}if(last)break;}return new Uint8Array(out);}window.hprofInflate=inflate;var dec=new TextDecoder("utf-8");window.hprofDecodeText=function(b64){return inflate(b64).then(function(u8){return dec.decode(u8);});};var dataEl=document.getElementById("report-data");window.__HPROF_DATA_B64__=dataEl?dataEl.textContent.trim():"";var bundleEl=document.getElementById("app-bundle");var bundleB64=bundleEl?bundleEl.textContent.trim():"";window.hprofDecodeText(bundleB64).then(function(src){var s=document.createElement("script");s.textContent=src;document.body.appendChild(s);}).catch(function(e){var fb=document.getElementById("hprof-fallback");if(fb)fb.textContent="Failed to load report bundle: "+e;});})();"#;

/// Run full analysis returning (Report, per-object retained-size vec).
pub fn analyze_to_report_with_retained(
    source: &crate::source::HprofSource,
    opts: &AnalyzeOptions,
) -> std::io::Result<(crate::report::Report, Vec<u64>)> {
    analyze_to_report_inner(source, opts, &mut |_, _| {})
}

/// Like `analyze_to_report_with_retained` but fires `progress(phase, fraction)`
/// at key boundaries so callers can update a progress indicator.
/// Phases fired: "pass1", "pass2", "rpo", "inbound", "dominators", "retained".
pub fn analyze_to_report_with_progress(
    source: &crate::source::HprofSource,
    opts: &AnalyzeOptions,
    progress: &mut dyn FnMut(&str, f32),
) -> std::io::Result<(crate::report::Report, Vec<u64>)> {
    analyze_to_report_inner(source, opts, progress)
}

/// Data retained after `build_exploration`, used for per-object BFS queries.
pub struct ExplorationResult {
    /// Block-sampled byte offsets into `inb_data`; one entry per INB_BLOCK nodes.
    pub inb_block_off: Vec<u64>,
    /// Vbyte-encoded inbound parent lists.
    pub inb_data: Vec<u8>,
    /// Dense indices that are GC roots (BFS stop condition).
    pub gc_root_set: std::collections::HashSet<u32>,
    /// GC root dense indices (1:1 with gc_root_types).
    pub gc_root_indices: Vec<u32>,
    /// GC root type tag per gc_root_index.
    pub gc_root_types: Vec<u8>,
    /// Object count.
    pub n: usize,
    /// Dense index → retained heap (bytes). May be zeros if retained not computed.
    pub retained: Vec<u64>,
    /// Dense index → class name string.
    pub class_names_by_idx: Vec<String>,
    /// Dense index → shallow heap (u32).
    pub shallow: Vec<u32>,
    /// pre-order → dense index mapping (rpo.vertex).
    pub rpo_vertex: Vec<u32>,
    /// Dense index → pre-order (inverse of rpo_vertex; u32::MAX = not in RPO).
    pub dense_to_pre: Vec<u32>,
    /// INB_BLOCK constant (needed by decoder).
    pub inb_block: usize,
    /// Forward CSR row pointers (len n+1). fwd_offsets[i]..fwd_offsets[i+1] slices
    /// fwd_targets for object i's out-edges.
    pub fwd_offsets: Vec<u32>,
    /// Forward CSR edge targets (parallel to fwd_field_name_idx when Some).
    pub fwd_targets: Vec<u32>,
    /// Per-edge field-name pool index, parallel to fwd_targets.
    /// None when --ref-paths was not used (all edges unnamed).
    pub fwd_field_name_idx: Option<Vec<u16>>,
    /// Deduped field-name strings; index 0 is always "" (unnamed).
    pub field_name_pool: Vec<String>,
    /// Dense index → HPROF object address (memory address). May be empty if
    /// id_map was already freed before `build_exploration` was called.
    pub addrs: Vec<u64>,
}

/// Build inbound CSR for interactive exploration (no report, no dominators).
/// Called by the WASM session's `enable_exploration()`.
pub fn build_exploration(
    source: &crate::source::HprofSource,
    retained: &[u64],
) -> std::io::Result<ExplorationResult> {
    let p1 = pass1::Pass1::run(source, false)?;
    let n = p1.class_ids.len();

    let compress = cvec::Codec::Deflate9;
    let opts = AnalyzeOptions::default();
    let mut no_in_sets = std::collections::HashMap::new();
    let mut no_exists_bools = std::collections::HashMap::new();
    let (
        mut g,
        mut inbound,
        shallow_c,
        class_idx_c,
        _alloc_serial_c,
        _query_state,
        _refwalk_csr,
        _string_values,
        _string_values_truncated,
    ) = pass2::Pass2::build(
        source,
        p1,
        compress,
        &opts,
        &[],
        &mut no_in_sets,
        &mut no_exists_bools,
    )?;

    // Extract object addresses before id_map is compressed away.
    let addrs: Vec<u64> = if let Some(ref m) = inbound.id_map {
        (0..n).map(|i| m.addr_at(i)).collect()
    } else {
        vec![]
    };

    inbound.compress_id_map(compress)?;

    let rpo = rpo_dfs::rpo_dfs(n, &g.gc_root_indices, &g.fwd_offsets, &g.fwd_targets);

    // Clone forward CSR before it's consumed by build_from_fwd.
    let saved_fwd_offsets: Vec<u32> = g.fwd_offsets.clone();
    let total_edges = g.fwd_offsets.last().copied().unwrap_or(0) as usize;
    let saved_fwd_targets: Vec<u32> = (0..total_edges).map(|i| g.fwd_targets.get(i)).collect();
    let saved_fwd_field_name_idx: Option<Vec<u16>> = g.fwd_field_name_idx.clone();
    let saved_field_name_pool: Vec<String> = g
        .field_name_pool
        .clone()
        .unwrap_or_else(|| vec![String::new()]);

    let (inb_block_off, inb_data) = inbound.build_from_fwd(
        std::mem::take(&mut g.fwd_offsets),
        std::mem::take(&mut g.fwd_targets),
        &rpo.dfn,
    )?;

    // Rebuild vertex from dfn before we lose dfn
    let rpo_vertex = rpo_dfs::rebuild_vertex(&rpo.dfn, rpo.parent_pre.len());

    // Build dense_to_pre inverse mapping
    let mut dense_to_pre = vec![u32::MAX; n];
    for (pre, &dense) in rpo_vertex.iter().enumerate() {
        if (dense as usize) < n {
            dense_to_pre[dense as usize] = pre as u32;
        }
    }

    let gc_root_set: std::collections::HashSet<u32> = g.gc_root_indices.iter().copied().collect();

    // Decompress shallow + class_idx for class name lookup
    let shallow: Vec<u32> = shallow_c.restore()?;
    let class_idx: Vec<u32> = class_idx_c.restore()?;

    let class_names_by_idx: Vec<String> = class_idx
        .iter()
        .map(|&ci| {
            let ci = ci as usize;
            if ci < g.class_names.len() {
                g.class_names[ci]
                    .replace('/', ".")
                    .replace("[[", "[")
                    .to_string()
            } else {
                format!("obj#{}", ci)
            }
        })
        .collect();

    let retained_vec: Vec<u64> = if retained.len() == n {
        retained.to_vec()
    } else {
        vec![0u64; n]
    };

    Ok(ExplorationResult {
        inb_block_off,
        inb_data,
        gc_root_set,
        gc_root_indices: g.gc_root_indices.clone(),
        gc_root_types: g.gc_root_types.clone(),
        n,
        retained: retained_vec,
        class_names_by_idx,
        shallow,
        rpo_vertex,
        dense_to_pre,
        inb_block: pass2::INB_BLOCK,
        fwd_offsets: saved_fwd_offsets,
        fwd_targets: saved_fwd_targets,
        fwd_field_name_idx: saved_fwd_field_name_idx,
        field_name_pool: saved_field_name_pool,
        addrs,
    })
}

fn analyze_to_report_inner(
    source: &crate::source::HprofSource,
    opts: &AnalyzeOptions,
    progress: &mut dyn FnMut(&str, f32),
) -> std::io::Result<(crate::report::Report, Vec<u64>)> {
    use std::io;
    let p1 = pass1::Pass1::run(source, false)?;
    let truncated_input = p1.truncated_input;
    progress("pass1", 1.0);

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
    ) = pass2::Pass2::build(
        source,
        p1,
        compress,
        opts,
        &[],
        &mut no_in_sets,
        &mut no_exists_bools,
    )?;
    progress("pass2", 1.0);

    inbound.compress_id_map(compress)?;

    let rpo = rpo_dfs::rpo_dfs(g.n, &g.gc_root_indices, &g.fwd_offsets, &g.fwd_targets);
    progress("rpo", 1.0);

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

    // ── Normal path ─────────────────────────────────────────────────────────────

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
    // When --field-stats is requested, save the fwd CSR before inbound consumes it.
    // build_field_stats is called right after retained is populated, then the copy is freed
    // before build_model to avoid keeping ~2 GB extra through that function's allocations.
    let field_stats_fwd: Option<(Vec<u32>, crate::chunkvec::ChunkU32)> = if opts.field_stats {
        let total_edges = g.fwd_offsets.last().copied().unwrap_or(0) as usize;
        let fwd_off_copy = g.fwd_offsets.clone();
        let mut fwd_tgt_copy = crate::chunkvec::ChunkU32::zeroed(total_edges);
        for i in 0..total_edges {
            fwd_tgt_copy.set(i, g.fwd_targets.get(i));
        }
        Some((fwd_off_copy, fwd_tgt_copy))
    } else {
        None
    };
    let (inb_block_off, inb_data) = inbound.build_from_fwd(
        std::mem::take(&mut g.fwd_offsets),
        std::mem::take(&mut g.fwd_targets),
        &rpo.dfn,
    )?;
    progress("inbound", 1.0);

    // Rebuild vertex while dfn is still live; then free dfn.
    let count = parent_pre_count;
    rpo.vertex = rpo_dfs::rebuild_vertex(&rpo.dfn, count);
    rpo.dfn = Vec::new();

    if let Some(c) = parent_pre_c {
        rpo.parent_pre = c.restore()?;
    }

    g.idom =
        dominator::compute_dominators(g.n, rpo, &g.gc_root_indices, &inb_block_off, &inb_data)?;
    progress("dominators", 1.0);
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
        &g.shallow,
        &g.class_idx,
        class_count,
        &g.class_obj_class_idx,
        &dc_off,
        &dc_tgt,
    );
    progress("retained", 1.0);
    g.retained = retained;
    g.has_same_class_ancestor = has_same;

    // If --field-stats was requested, compute field_stats now using the saved fwd
    // copy, then drop the copy immediately. This frees the ~2 GB clone before
    // build_model runs, reducing the peak RSS compared to restoring it into g and
    // keeping it alive through all the heavy build_model allocations.
    let precomputed_field_stats: Option<crate::report::FieldStats> =
        if let Some((fwd_off, fwd_tgt)) = field_stats_fwd {
            // Temporarily place saved fwd data into g for build_field_stats.
            g.fwd_offsets = fwd_off;
            g.fwd_targets = fwd_tgt;
            let fs = crate::report::build_field_stats(&g);
            // Immediately free the copy — we no longer need it.
            g.fwd_offsets = Vec::new();
            g.fwd_targets = crate::chunkvec::ChunkU32::default();
            Some(fs)
        } else {
            None
        };

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

    let mut report = report::build_model(
        &mut g,
        dc_off,
        dc_tgt,
        opts.leak_children_cap,
        &depth_counts,
        opts,
        alloc_sites,
        precomputed_field_stats,
    );
    report.truncated_input = truncated_input;
    // dc_off and dc_tgt were moved into build_model and freed early inside it.

    // Extract the per-object retained-size array before g is dropped.
    // The caller (analyze_to_report_with_retained) stores this for OQL reuse.
    let retained = std::mem::take(&mut g.retained);

    Ok((report, retained))
}

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

    static BUNDLE_DEFLATED: &[u8] =
        include_bytes!(concat!(env!("OUT_DIR"), "/bundle.deflate"));

    fn b64(bytes: &[u8]) -> String {
        base64::engine::general_purpose::STANDARD.encode(bytes)
    }

    fn deflate_b64(bytes: &[u8]) -> String {
        let mut enc = flate2::write::DeflateEncoder::new(
            Vec::new(), flate2::Compression::new(9));
        enc.write_all(bytes).expect("deflate write");
        b64(&enc.finish().expect("deflate finish"))
    }

    let data_b64 = deflate_b64(report_json.as_bytes());
    let bundle_b64 = b64(BUNDLE_DEFLATED);

    let title = source_name
        .replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");

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
    analyze_to_report_inner(source, opts)
}

fn analyze_to_report_inner(
    source: &crate::source::HprofSource,
    opts: &AnalyzeOptions,
) -> std::io::Result<(crate::report::Report, Vec<u64>)> {
    use std::io;
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

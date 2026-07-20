//! Self-contained HTML report renderer.
//!
//! `render_html` emits a SINGLE self-contained HTML file with NO network
//! dependencies. Everything heavy is embedded COMPRESSED (raw-DEFLATE via
//! `flate2`, then base64) and inflated CLIENT-SIDE at load via the browser's
//! `DecompressionStream('deflate-raw')`:
//!
//!   - the report JSON (in a `<script type="application/octet-stream"
//!     id="report-data">` blob), and
//!   - the React app bundle (JS + inlined CSS).
//!
//! The ONLY uncompressed JS in the file is a tiny bootstrap that base64-decodes
//! and inflates the bundle blob, injects it as a `<script>` to boot the app,
//! which then decodes + inflates + parses the report-data blob and renders.
//!
//! flate2's `DeflateEncoder` produces RAW deflate (no zlib/gzip header), which
//! matches `DecompressionStream('deflate-raw')` end-to-end.

use std::io::Write;
use std::sync::OnceLock;

use base64::Engine as _;
use flate2::{Compression, write::DeflateEncoder};

use crate::diff_reports::SeriesDiffResult;
use crate::report::Report;
/// The React bundle pre-compressed as raw-deflate by `build.rs`.
/// base64-encoded directly into the HTML; the browser inflates it via
/// `DecompressionStream('deflate-raw')`.
static BUNDLE_DEFLATED: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/bundle.deflate"));

fn bundle_b64() -> &'static str {
    static CACHED: OnceLock<String> = OnceLock::new();
    CACHED.get_or_init(|| base64::engine::general_purpose::STANDARD.encode(BUNDLE_DEFLATED))
}

/// Raw-DEFLATE (level 9) then base64-encode a byte slice. The codec matches the
/// analyzer's `--format json` Deflate9 and the browser's `deflate-raw`.
fn deflate_b64(bytes: &[u8]) -> String {
    let mut enc = DeflateEncoder::new(Vec::new(), Compression::new(9));
    enc.write_all(bytes)
        .expect("deflate write to Vec is infallible");
    let compressed = enc.finish().expect("deflate finish to Vec is infallible");
    base64::engine::general_purpose::STANDARD.encode(compressed)
}

/// Render a `Report` to a single self-contained HTML document.
///
/// Deterministic: for a given `Report` the output is byte-identical across
/// runs (serde_json preserves field order, the model carries only sorted
/// vectors, and deflate/base64 are pure functions of their input).
pub fn render_html(r: &Report) -> String {
    // Report JSON: same shape as `--format json` (compact here; the client
    // JSON.parses it — pretty-printing would only bloat the compressed blob).
    let json = serde_json::to_string(r).expect("Report serializes to JSON");
    let data_b64 = deflate_b64(json.as_bytes());
    let bundle_b64 = bundle_b64();

    let title = format!("Heap Dump Analysis: {}", r.overview.source_name);
    let title = html_escape(&title);

    // The bootstrap is the ONLY uncompressed JS. It reads the two base64 blobs
    // from the DOM, inflates the bundle blob (deflate-raw) to JS text, and
    // injects it as a <script> so the app boots; the app then reads the
    // compressed report blob, inflates + JSON.parses it, and renders. A
    // pure-JS inflate fallback covers browsers lacking DecompressionStream
    // (older Safari/Firefox), so the offline file always opens.
    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{title}</title>
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
<script>{bootstrap}</script>
</body>
</html>
"#,
        title = title,
        data_b64 = data_b64,
        bundle_b64 = bundle_b64,
        bootstrap = BOOTSTRAP_JS,
    )
}

/// Render an N-way cross-dump `SeriesDiffResult` to a single self-contained
/// HTML document. Reuses the SAME embedded React bundle and bootstrap as
/// `render_html`; the ONLY difference is the payload placed in `#report-data`.
///
/// Where a single-dump report embeds the RAW report JSON, this embeds a tagged
/// envelope `{"kind":"series-diff","diff": <SeriesDiffResult>}` so the shared
/// bundle can dispatch report-vs-diff at boot. A real single-dump `Report` has
/// no `kind` field, so the branch is unambiguous and backward-compatible.
///
/// Deterministic: for a given diff the output is byte-identical across runs.
pub fn render_diff_html(d: &SeriesDiffResult) -> String {
    // Tagged envelope so the shared bundle can tell a diff from a report.
    let envelope = serde_json::json!({ "kind": "series-diff", "diff": d });
    let json = serde_json::to_string(&envelope).expect("diff envelope serializes to JSON");
    let data_b64 = deflate_b64(json.as_bytes());
    let bundle_b64 = bundle_b64();

    let title = format!("Heap Dump Comparison ({} reports)", d.labels.len());
    let title = html_escape(&title);

    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{title}</title>
<style>
:root {{ color-scheme: light dark; }}
html, body {{ margin: 0; padding: 0; font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, Helvetica, Arial, sans-serif; }}
#root {{ padding: 0; }}
#hprof-fallback {{ padding: 1rem; }}
</style>
</head>
<body>
<div id="root"><div id="hprof-fallback">Loading heap dump comparison&hellip;</div></div>
<script type="application/octet-stream" id="report-data">{data_b64}</script>
<script type="application/octet-stream" id="app-bundle">{bundle_b64}</script>
<script>{bootstrap}</script>
</body>
</html>
"#,
        title = title,
        data_b64 = data_b64,
        bundle_b64 = bundle_b64,
        bootstrap = BOOTSTRAP_JS,
    )
}

/// Minimal HTML text escaper for the `<title>` (the only place untrusted model
/// text lands in raw HTML; all other data flows through the JSON blob and is
/// rendered via the DOM API in the app, never as raw HTML).
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// The uncompressed bootstrap loader. Kept tiny (~1-2 KB). Exposes
/// `window.hprofInflate(b64) -> Promise<Uint8Array>` and
/// `window.hprofDecodeText(b64) -> Promise<String>` used by both the bootstrap
/// (for the bundle) and the app (for the report data), then boots the bundle.
const BOOTSTRAP_JS: &str = r#"
(function () {
  function b64ToBytes(b64) {
    var bin = atob(b64);
    var len = bin.length;
    var out = new Uint8Array(len);
    for (var i = 0; i < len; i++) out[i] = bin.charCodeAt(i);
    return out;
  }
  async function inflate(b64) {
    var bytes = b64ToBytes(b64);
    if (typeof DecompressionStream === "function") {
      var ds = new DecompressionStream("deflate-raw");
      var stream = new Response(new Blob([bytes]).stream().pipeThrough(ds));
      var buf = await stream.arrayBuffer();
      return new Uint8Array(buf);
    }
    return tinfl(bytes);
  }
  // Pure-JS raw-DEFLATE fallback for browsers without DecompressionStream.
  // Load-bearing: the offline file must always open.
  function tinfl(input) {
    var out = [], op = 0, ip = 0, bitBuf = 0, bitCnt = 0;
    function need(n){ while(bitCnt<n){ bitBuf|=input[ip++]<<bitCnt; bitCnt+=8; } }
    function bits(n){ need(n); var v=bitBuf&((1<<n)-1); bitBuf>>=n; bitCnt-=n; return v; }
    function build(lens){
      var max=0; for(var i=0;i<lens.length;i++) if(lens[i]>max) max=lens[i];
      var cnt=new Array(max+1).fill(0); for(i=0;i<lens.length;i++) cnt[lens[i]]++;
      cnt[0]=0; var next=new Array(max+1).fill(0), code=0;
      for(i=1;i<=max;i++){ code=(code+cnt[i-1])<<1; next[i]=code; }
      var codes={}; for(i=0;i<lens.length;i++){ var l=lens[i]; if(l){ codes[l+"_"+next[l]]=i; next[l]++; } }
      return {codes:codes,max:max};
    }
    function decode(t){ var code=0; for(var l=1;l<=t.max;l++){ code=(code<<1)|bits(1); var s=t.codes[l+"_"+code]; if(s!==undefined) return s; } throw "bad code"; }
    var LB=[3,4,5,6,7,8,9,10,11,13,15,17,19,23,27,31,35,43,51,59,67,83,99,115,131,163,195,227,258];
    var LE=[0,0,0,0,0,0,0,0,1,1,1,1,2,2,2,2,3,3,3,3,4,4,4,4,5,5,5,5,0];
    var DB=[1,2,3,4,5,7,9,13,17,25,33,49,65,97,129,193,257,385,513,769,1025,1537,2049,3073,4097,6145,8193,12289,16385,24577];
    var DE=[0,0,0,0,1,1,2,2,3,3,4,4,5,5,6,6,7,7,8,8,9,9,10,10,11,11,12,12,13,13];
    var CLO=[16,17,18,0,8,7,9,6,10,5,11,4,12,3,13,2,14,1,15];
    while(true){
      var last=bits(1), type=bits(2);
      if(type===0){ bitBuf=0; bitCnt=0; var lenv=input[ip]|(input[ip+1]<<8); ip+=4; for(var k=0;k<lenv;k++) out[op++]=input[ip++]; }
      else {
        var lt, dt;
        if(type===1){
          var ll=[]; for(var i=0;i<288;i++) ll.push(i<144?8:i<256?9:i<280?7:8);
          var dl=[]; for(i=0;i<30;i++) dl.push(5);
          lt=build(ll); dt=build(dl);
        } else {
          var hlit=bits(5)+257, hdist=bits(5)+1, hclen=bits(4)+4;
          var cl=new Array(19).fill(0); for(i=0;i<hclen;i++) cl[CLO[i]]=bits(3);
          var ct=build(cl); var all=[]; while(all.length<hlit+hdist){ var s=decode(ct);
            if(s<16) all.push(s); else if(s===16){ var r=bits(2)+3, p=all[all.length-1]; while(r--) all.push(p); }
            else if(s===17){ var r2=bits(3)+3; while(r2--) all.push(0); }
            else { var r3=bits(7)+11; while(r3--) all.push(0); } }
          lt=build(all.slice(0,hlit)); dt=build(all.slice(hlit));
        }
        while(true){ var sym=decode(lt);
          if(sym===256) break;
          if(sym<256){ out[op++]=sym; }
          else { sym-=257; var length=LB[sym]+bits(LE[sym]); var ds2=decode(dt); var dist=DB[ds2]+bits(DE[ds2]);
            for(var c=0;c<length;c++){ out[op]=out[op-dist]; op++; } }
        }
      }
      if(last) break;
    }
    return new Uint8Array(out);
  }
  window.hprofInflate = inflate;
  var dec = new TextDecoder("utf-8");
  window.hprofDecodeText = function (b64) { return inflate(b64).then(function (u8) { return dec.decode(u8); }); };
  var dataEl = document.getElementById("report-data");
  window.__HPROF_DATA_B64__ = dataEl ? dataEl.textContent.trim() : "";
  var bundleEl = document.getElementById("app-bundle");
  var bundleB64 = bundleEl ? bundleEl.textContent.trim() : "";
  window.hprofDecodeText(bundleB64).then(function (src) {
    var s = document.createElement("script");
    s.textContent = src;
    document.body.appendChild(s);
  }).catch(function (e) {
    var fb = document.getElementById("hprof-fallback");
    if (fb) fb.textContent = "Failed to load report bundle: " + e;
  });
})();
"#;

#!/usr/bin/env bash
# scripts/build-browser.sh — build dist/hprof-analyzer-browser.html
# Embeds the WASM module (base64), WASM glue JS, shell JS, and CSS into a
# single self-contained HTML file.  xterm.js is loaded from CDN at runtime.
#
# Usage: bash scripts/build-browser.sh
#        (or: bash scripts/build-browser.sh --skip-wasm-build  to reuse existing pkg/)

set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

SKIP_BUILD=0
for arg in "$@"; do
  [[ "$arg" == "--skip-wasm-build" ]] && SKIP_BUILD=1
done

if [[ $SKIP_BUILD -eq 0 ]]; then
  echo "Building WASM module (release)..."
  wasm-pack build crates/hprof-wasm --target web --release
else
  echo "Skipping WASM build (--skip-wasm-build)."
fi

echo "Assembling dist/hprof-analyzer-browser.html..."
mkdir -p dist

python3 - <<'PYEOF'
import base64, sys, os, textwrap

def read(path):
    with open(path, encoding='utf-8') as f:
        return f.read()

def readb64(path):
    with open(path, 'rb') as f:
        return base64.b64encode(f.read()).decode()

# Sanity-check inputs exist
for p in [
    'web-browser/index.html',
    'web-browser/shell.js',
    'web-browser/style.css',
    'web/dist/bundle.js',
    'crates/hprof-wasm/pkg/hprof_wasm.js',
    'crates/hprof-wasm/pkg/hprof_wasm_bg.wasm',
]:
    if not os.path.exists(p):
        print(f"ERROR: required file not found: {p}", file=sys.stderr)
        sys.exit(1)

html = read('web-browser/index.html')

# Prepare embedded JS for inlining into an HTML <script> element.
#   1. Neutralise HTML-parser-significant token sequences. A bare `<script`,
#      `</script`, or `<!--` inside a <script> element (even one holding minified
#      library code, e.g. jQuery's `"<script><\/script>"` feature probe) flips the
#      HTML tokenizer into a script-data-escaped state and prevents the tag from
#      closing — silently swallowing everything after it. A backslash after `<` is
#      inert in JS strings/regex but hides the token from the HTML parser.
#   2. Escape every non-ASCII code point to a `\uXXXX` sequence. Large inline
#      scripts served over HTTP can have a multibyte UTF-8 char split across a
#      network/parse chunk boundary, which some browsers mis-decode into a lone
#      surrogate → "Invalid or unexpected token" and the whole module fails to run.
#      Pure-ASCII source is immune (the escapes decode to the same chars at parse
#      time, so behaviour is unchanged).
import re
def prep_embedded_js(js):
    js = re.sub(r'<(/?script|!--)', r'<\\\1', js, flags=re.IGNORECASE)
    out = []
    for ch in js:
        o = ord(ch)
        if o < 0x80:
            out.append(ch)
        elif o <= 0xFFFF:
            out.append('\\u%04x' % o)
        else:
            hi = 0xD800 + ((o - 0x10000) >> 10)
            lo = 0xDC00 + ((o - 0x10000) & 0x3FF)
            out.append('\\u%04x\\u%04x' % (hi, lo))
    return ''.join(out)

wasm_js = read('crates/hprof-wasm/pkg/hprof_wasm.js')
# The glue is stored verbatim in an inert <script type="text/plain"> tag (read back
# via .textContent and imported as a blob-URL module), NOT inside a template
# literal — so it must NOT be backslash-escaped for `` ` `` / ${ }. It still needs
# the HTML-token neutralisation + ASCII escaping that prep_embedded_js provides.
html = html.replace('%%WASM_JS%%',  prep_embedded_js(wasm_js))
html = html.replace('%%WASM_B64%%', readb64('crates/hprof-wasm/pkg/hprof_wasm_bg.wasm'))
html = html.replace('%%SHELL_JS%%', prep_embedded_js(read('web-browser/shell.js')))
html = html.replace('%%STYLE_CSS%%', read('web-browser/style.css'))
html = html.replace('%%REACT_BUNDLE%%', prep_embedded_js(read('web/dist/bundle.js')))

out = 'dist/hprof-analyzer-browser.html'
with open(out, 'w', encoding='utf-8') as f:
    f.write(html)

size_kb = len(html.encode('utf-8')) // 1024
wasm_size_kb = os.path.getsize('crates/hprof-wasm/pkg/hprof_wasm_bg.wasm') // 1024
print(f"Built: {out} ({size_kb} KB total, WASM binary {wasm_size_kb} KB)")
PYEOF

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
    'crates/hprof-wasm/pkg/hprof_wasm.js',
    'crates/hprof-wasm/pkg/hprof_wasm_bg.wasm',
]:
    if not os.path.exists(p):
        print(f"ERROR: required file not found: {p}", file=sys.stderr)
        sys.exit(1)

html = read('web-browser/index.html')
wasm_js = read('crates/hprof-wasm/pkg/hprof_wasm.js')
# Escape characters that would corrupt the enclosing template literal in index.html
wasm_js_escaped = wasm_js.replace('\\', '\\\\').replace('`', '\\`').replace('${', '\\${')
html = html.replace('%%WASM_JS%%',  wasm_js_escaped)
html = html.replace('%%WASM_B64%%', readb64('crates/hprof-wasm/pkg/hprof_wasm_bg.wasm'))
html = html.replace('%%SHELL_JS%%', read('web-browser/shell.js'))
html = html.replace('%%STYLE_CSS%%', read('web-browser/style.css'))

out = 'dist/hprof-analyzer-browser.html'
with open(out, 'w', encoding='utf-8') as f:
    f.write(html)

size_kb = len(html.encode('utf-8')) // 1024
wasm_size_kb = os.path.getsize('crates/hprof-wasm/pkg/hprof_wasm_bg.wasm') // 1024
print(f"Built: {out} ({size_kb} KB total, WASM binary {wasm_size_kb} KB)")
PYEOF

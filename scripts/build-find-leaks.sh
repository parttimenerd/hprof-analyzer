#!/usr/bin/env bash
# scripts/build-find-leaks.sh — install docs/find-leaks/ from web-find-leaks/
# Also installs shared WASM assets to docs/wasm/ (reused by find-leaks page).
#
# Usage: bash scripts/build-find-leaks.sh

set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

# ── Shared WASM assets ───────────────────────────────────────────────────────
# find-leaks loads WASM via relative URL (../wasm/) rather than embedding it,
# keeping the page small (< 20 KB vs 5+ MB for the embedded browser tool).
WASM_PKG=crates/hprof-wasm/pkg
if [[ ! -f "$WASM_PKG/hprof_wasm.js" ]]; then
  echo "ERROR: $WASM_PKG/hprof_wasm.js not found. Run: wasm-pack build crates/hprof-wasm --target web --release"
  exit 1
fi

echo "Installing shared WASM assets → docs/wasm/"
mkdir -p docs/wasm
cp "$WASM_PKG/hprof_wasm.js"       docs/wasm/
cp "$WASM_PKG/hprof_wasm_bg.wasm"  docs/wasm/

# ── find-leaks page ──────────────────────────────────────────────────────────
echo "Installing find-leaks page → docs/find-leaks/"
mkdir -p docs/find-leaks
cp web-find-leaks/index.html docs/find-leaks/index.html

echo "Done."
echo "  docs/wasm/         — shared WASM assets ($(du -sh docs/wasm | cut -f1) total)"
echo "  docs/find-leaks/   — secret finder page"

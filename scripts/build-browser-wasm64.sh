#!/usr/bin/env bash
# scripts/build-browser-wasm64.sh — build the EXPERIMENTAL wasm64 (memory64)
# variant of the WASM module and emit it as fetchable assets under dist/wasm64/.
#
# The default browser build (scripts/build-browser.sh) is wasm32, whose linear
# memory is capped at 4 GiB. This wasm64 build lifts that cap so the in-browser
# analyzer can load larger heap dumps. The default page fetches these assets
# in-place when a file is too big (see web-browser/shell.js fallback path).
#
# REQUIREMENTS (experimental, opt-in — NOT part of the default cargo build):
#   * A Rust *nightly* toolchain with the `rust-src` component:
#       rustup toolchain install nightly --component rust-src
#   * wasm64-unknown-unknown is a Tier-3 target with no prebuilt std, so we
#     build std from source via -Z build-std.
#   * wasm-bindgen 0.2.127 (matches Cargo.lock) — reused from the wasm-pack cache.
#
# This script overrides the toolchain per-invocation with RUSTUP_TOOLCHAIN and
# does NOT modify rust-toolchain.toml (the repo stays pinned to stable).
#
# Usage: bash scripts/build-browser-wasm64.sh

set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

NIGHTLY="${HPROF_NIGHTLY_TOOLCHAIN:-nightly}"
WB_VERSION="0.2.127"
OUT_DIR="dist/wasm64"

echo "==> Checking prerequisites…"
if ! rustup toolchain list 2>/dev/null | grep -q "$NIGHTLY"; then
  echo "ERROR: nightly toolchain '$NIGHTLY' not installed." >&2
  echo "       Run: rustup toolchain install nightly --component rust-src" >&2
  exit 1
fi
if ! rustup component list --toolchain "$NIGHTLY" 2>/dev/null | grep -q "rust-src (installed)"; then
  echo "ERROR: rust-src not installed for '$NIGHTLY'." >&2
  echo "       Run: rustup component add rust-src --toolchain $NIGHTLY" >&2
  exit 1
fi

# Locate the wasm-bindgen CLI matching Cargo.lock (reuse the wasm-pack cache, or
# fall back to a wasm-bindgen on PATH if its version matches).
WB=""
for cand in \
  "$HOME/Library/Caches/.wasm-pack/wasm-bindgen-cargo-install-${WB_VERSION}/wasm-bindgen" \
  "$HOME/.cache/.wasm-pack/wasm-bindgen-cargo-install-${WB_VERSION}/wasm-bindgen"; do
  [[ -x "$cand" ]] && WB="$cand" && break
done
if [[ -z "$WB" ]] && command -v wasm-bindgen >/dev/null 2>&1; then
  if wasm-bindgen --version 2>/dev/null | grep -q "$WB_VERSION"; then
    WB="$(command -v wasm-bindgen)"
  fi
fi
if [[ -z "$WB" ]]; then
  echo "ERROR: wasm-bindgen $WB_VERSION not found." >&2
  echo "       Build the wasm32 page once (scripts/build-browser.sh) to populate" >&2
  echo "       the wasm-pack cache, or: cargo install wasm-bindgen-cli --version $WB_VERSION" >&2
  exit 1
fi
echo "    wasm-bindgen: $WB ($("$WB" --version))"

echo "==> Building hprof-wasm for wasm64-unknown-unknown (nightly + build-std)…"
RUSTUP_TOOLCHAIN="$NIGHTLY" cargo build \
  -p hprof-wasm \
  --target wasm64-unknown-unknown \
  -Z build-std=std,panic_abort \
  --release

RAW_WASM="target/wasm64-unknown-unknown/release/hprof_wasm.wasm"
if [[ ! -f "$RAW_WASM" ]]; then
  echo "ERROR: expected build artifact not found: $RAW_WASM" >&2
  exit 1
fi

echo "==> Running wasm-bindgen (--target web)…"
mkdir -p "$OUT_DIR"
"$WB" --target web --out-dir "$OUT_DIR" "$RAW_WASM"

# wasm-opt with memory64 enabled (best-effort; skip if the installed wasm-opt is
# too old to understand the memory64 feature).
BG_WASM="$OUT_DIR/hprof_wasm_bg.wasm"
if command -v wasm-opt >/dev/null 2>&1; then
  echo "==> Optimizing with wasm-opt (--enable-memory64)…"
  if wasm-opt --enable-memory64 -O2 "$BG_WASM" -o "$BG_WASM.opt" 2>/dev/null; then
    mv "$BG_WASM.opt" "$BG_WASM"
  else
    echo "    (wasm-opt does not support memory64 on this system — skipping optimization)"
    rm -f "$BG_WASM.opt"
  fi
fi

JS_KB=$(( $(wc -c < "$OUT_DIR/hprof_wasm.js") / 1024 ))
WASM_KB=$(( $(wc -c < "$BG_WASM") / 1024 ))
echo "==> Done. Experimental wasm64 assets in $OUT_DIR/:"
echo "    hprof_wasm.js       ${JS_KB} KB"
echo "    hprof_wasm_bg.wasm  ${WASM_KB} KB"
echo
echo "Serve dist/ over HTTP; the default page fetches ./wasm64/ on demand for"
echo "files too large for the wasm32 build."

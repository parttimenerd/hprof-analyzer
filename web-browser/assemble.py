#!/usr/bin/env python3
"""
Assemble the self-contained hprof-analyzer browser bundle.

Usage (from repo root):
    python3 web-browser/assemble.py [--output dist/hprof-analyzer-browser.html]

Reads:
  crates/hprof-wasm/pkg/hprof_wasm_bg.wasm   — compiled WASM binary
  crates/hprof-wasm/pkg/hprof_wasm.js        — wasm-pack JS glue
  web-browser/index.html                      — HTML template (%%MARKERS%%)
  web-browser/shell.js                        — terminal shell logic
  web-browser/style.css                       — CSS

The WASM glue is embedded in a <script type="text/plain"> tag (not a template
literal) to avoid escaping issues with backticks in JSDoc comments.

The WASM binary is base64-encoded and stored in a second inert script tag.
At runtime JS reads both tags via document.getElementById() and imports the
glue as a blob-URL ES module.

Prerequisites:
  wasm-pack build crates/hprof-wasm --target web --release
"""

import argparse
import base64
import pathlib
import sys


def assemble(output: pathlib.Path) -> None:
    root = pathlib.Path(__file__).parent.parent

    pkg = root / "crates/hprof-wasm/pkg"
    wasm_bin = pkg / "hprof_wasm_bg.wasm"
    wasm_js  = pkg / "hprof_wasm.js"
    react_bundle = root / "web/dist/bundle.js"

    for p in (wasm_bin, wasm_js):
        if not p.exists():
            sys.exit(
                f"Missing: {p}\n"
                "Run: wasm-pack build crates/hprof-wasm --target web --release"
            )
    if not react_bundle.exists():
        sys.exit(
            f"Missing: {react_bundle}\n"
            "Run: cd web && node esbuild.config.mjs"
        )

    wasm  = wasm_bin.read_bytes()
    wjs   = wasm_js.read_text(encoding="utf-8")
    shell = (root / "web-browser/shell.js").read_text(encoding="utf-8")
    css   = (root / "web-browser/style.css").read_text(encoding="utf-8")
    idx   = (root / "web-browser/index.html").read_text(encoding="utf-8")
    react = react_bundle.read_text(encoding="utf-8")

    b64wasm = base64.b64encode(wasm).decode("ascii")

    html = idx
    html = html.replace("%%STYLE_CSS%%", css)
    # WASM JS goes into an inert <script type="text/plain"> — no escaping needed.
    # The only HTML-special sequence to guard against is "</script>", which
    # wasm-pack JS has never emitted, but check anyway.
    if "</script" in wjs.lower():
        sys.exit("WASM JS contains </script> — escaping required; update this script.")
    html = html.replace("%%WASM_JS%%", wjs)
    html = html.replace("%%WASM_B64%%", b64wasm)
    html = html.replace("%%REACT_BUNDLE%%", react)
    html = html.replace("%%SHELL_JS%%", shell)

    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(html, encoding="utf-8")

    size_kb  = output.stat().st_size / 1024
    wasm_kb  = len(wasm) / 1024
    b64_kb   = len(b64wasm) / 1024
    react_kb = len(react.encode()) / 1024
    print(
        f"Written {output}  "
        f"({size_kb:.0f} KB total, wasm={wasm_kb:.0f} KB, b64={b64_kb:.0f} KB, react={react_kb:.0f} KB)"
    )


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument(
        "--output", "-o",
        type=pathlib.Path,
        default=pathlib.Path("dist/hprof-analyzer-browser.html"),
        help="Output path (default: dist/hprof-analyzer-browser.html)",
    )
    args = ap.parse_args()
    assemble(args.output)


if __name__ == "__main__":
    main()

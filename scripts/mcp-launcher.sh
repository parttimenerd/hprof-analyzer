#!/bin/sh
# G1: robust launcher for the hprof-analyzer MCP server.
#
# The repo's .mcp.json points here instead of at
# target/release/hprof-analyzer, which does not exist on a fresh checkout
# (ENOENT for the MCP client at startup). Resolution order:
#   1. `hprof-analyzer` on PATH (installed binary, e.g. via cargo install)
#   2. the repo-local release build (<repo>/target/release/hprof-analyzer)
# If neither exists, build the release binary once (cargo build --release)
# and use it.
set -eu

BIN=$(command -v hprof-analyzer 2>/dev/null || true)
REPO_DIR=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
LOCAL_BIN="$REPO_DIR/target/release/hprof-analyzer"

if [ -z "$BIN" ]; then
    if [ -x "$LOCAL_BIN" ]; then
        BIN="$LOCAL_BIN"
    else
        echo "mcp-launcher: hprof-analyzer not found." >&2
        echo "  neither 'hprof-analyzer' on PATH nor $LOCAL_BIN exists." >&2
        echo "  fix: run 'cargo build --release' in this repo, or 'cargo install --path .' to install it on PATH" >&2
        exit 127
    fi
fi

exec "$BIN" mcp "$@"

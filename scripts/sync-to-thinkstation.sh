#!/usr/bin/env bash
# Mirror the local hprof-analyzer source tree to thinkstation:~/hprof-analyzer
# and rebuild the release binary there. On-demand: run this before a remote
# big-dump run so the remote reflects the local working tree exactly.
#
# Mirrors ONLY source + build inputs (src/, Cargo.toml, Cargo.lock, schema/,
# scripts/, build.rs if present). Excludes target/ and proptest-regressions/.
# rsync --delete keeps the remote source tree an exact mirror of local — any
# remote-only edits (e.g. spent diagnostics) are discarded by design.
#
# Usage: scripts/sync-to-thinkstation.sh [--no-build]
set -euo pipefail

REMOTE=thinkstation
REMOTE_DIR='~/hprof-analyzer'
LOCAL_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

BUILD=1
[ "${1:-}" = "--no-build" ] && BUILD=0

# Refuse to sync while a big-dump analysis is running on the remote — an rsync
# mid-run is harmless to the already-loaded binary, but rebuilding would race
# the running process and a source swap is confusing to reason about.
if ssh "$REMOTE" 'pgrep -f "release/hprof-analyzer" >/dev/null 2>&1'; then
  echo "ABORT: an hprof-analyzer process is running on $REMOTE (big-dump run?)." >&2
  echo "       Wait for it to finish, or re-run after it exits." >&2
  exit 1
fi

echo "==> mirroring $LOCAL_DIR/ -> $REMOTE:$REMOTE_DIR/ (src + build inputs)"
rsync -az --delete \
  --include='src/***' \
  --include='schema/***' \
  --include='scripts/***' \
  --include='Cargo.toml' \
  --include='Cargo.lock' \
  --include='build.rs' \
  --exclude='*' \
  "$LOCAL_DIR/" "$REMOTE:$REMOTE_DIR/"

if [ "$BUILD" = "1" ]; then
  echo "==> cargo build --release on $REMOTE"
  # touch edited sources + main so the build sees them. Use a login shell
  # (bash -lc) so cargo resolves on PATH regardless of how it was installed
  # (rustup ~/.cargo/env OR a snap/system package under /snap/bin).
  ssh "$REMOTE" "bash -lc 'cd $REMOTE_DIR && \
    touch src/*.rs && cargo build --release 2>&1 | grep -E \"error|warning\" || true; \
    echo BUILD_DONE'"
fi

echo "==> sync complete"

#!/usr/bin/env bash
# Measure peak RSS + runtime of a release `analyze` run on the big dump, run
# DETACHED on thinkstation so it survives disconnects and does not tie up this
# shell. Reports the kernel's true peak (VmHWM via /usr/bin/time -v), which is
# the number every pipeline-touching commit message must record.
#
# Prerequisites: sync the source + rebuild the remote binary FIRST via
#   scripts/sync-to-thinkstation.sh
# (this script does NOT build — it runs the already-built remote binary).
#
# Usage:
#   scripts/measure-bigdump-rss.sh start     # launch detached, prints run id
#   scripts/measure-bigdump-rss.sh status    # is a run active? tail the log
#   scripts/measure-bigdump-rss.sh result    # print peak RSS + runtime summary
#
# Only ONE run at a time (big dump saturates memory bandwidth); `start` refuses
# if a run is already active.
set -euo pipefail

REMOTE=thinkstation
REMOTE_DIR='~/hprof-analyzer'
# NOTE: paths that get embedded inside the escaped-double-quote ssh payload
# below MUST NOT rely on a literal `~` — tilde does NOT expand inside double
# quotes, so `/usr/bin/time "~/.../bin"` fails with exit 127. The launch runs
# after `cd $REMOTE_DIR`, so BIN/OUT/LOG are RELATIVE to that dir, and DUMP
# uses $HOME (expanded by the remote shell) instead of `~`.
DUMP='"$HOME"/test-heapdumps/pc52bs2job-triage-c475689d4-brk4b-20260617_145230.hprof'
BIN='./target/release/hprof-analyzer'
LOG="$REMOTE_DIR/bigdump-rss.timev"
OUT='/dev/null'   # discard the report body; we only want RSS + runtime

cmd="${1:-status}"

case "$cmd" in
  start)
    # Refuse to double-launch: the big dump needs the whole machine.
    if ssh "$REMOTE" 'pgrep -f "release/hprof-analyzer" >/dev/null 2>&1'; then
      echo "ABORT: an hprof-analyzer run is already active on $REMOTE." >&2
      echo "       Use 'status' to watch it, or wait for it to finish." >&2
      exit 1
    fi
    echo "==> launching detached big-dump RSS run on $REMOTE"
    # setsid + nohup + </dev/null so the run is fully detached from this ssh
    # session and keeps going after disconnect. /usr/bin/time -v writes the
    # VmHWM ("Maximum resident set size") + wall clock to $LOG.
    ssh "$REMOTE" "bash -lc 'cd $REMOTE_DIR && \
      setsid nohup /usr/bin/time -v $BIN analyze $DUMP \"$OUT\" \
        > $REMOTE_DIR/bigdump-rss.stdout 2> $LOG < /dev/null & \
      echo LAUNCHED pid=\$!'"
    echo "==> run detached. Check progress with: $0 status"
    ;;

  status)
    if ssh "$REMOTE" 'pgrep -f "release/hprof-analyzer" >/dev/null 2>&1'; then
      echo "==> RUN ACTIVE on $REMOTE. Live /proc RSS:"
      ssh "$REMOTE" "bash -lc 'p=\$(pgrep -f release/hprof-analyzer | head -1); \
        awk \"/VmRSS/ {printf \\\"  VmRSS=%.0f MB\\\\n\\\", \\\$2/1024}\" /proc/\$p/status 2>/dev/null; \
        awk \"/VmHWM/ {printf \\\"  VmHWM=%.0f MB (peak so far)\\\\n\\\", \\\$2/1024}\" /proc/\$p/status 2>/dev/null'"
    else
      echo "==> no run active on $REMOTE (finished or never started)."
      echo "==> use '$0 result' for the final peak + runtime."
    fi
    ;;

  result)
    echo "==> $LOG (peak RSS + runtime):"
    ssh "$REMOTE" "bash -lc 'grep -E \"Maximum resident set size|Elapsed .wall clock|User time|System time\" $LOG 2>/dev/null || echo \"(no result log yet at $LOG)\"'"
    ;;

  *)
    echo "usage: $0 {start|status|result}" >&2
    exit 2
    ;;
esac

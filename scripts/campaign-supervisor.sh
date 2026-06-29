#!/usr/bin/env bash
# Campaign supervisor: runs the systematic + covfuzz engines as independent
# chunked loops until each reports done (exit 42). Each chunk checkpoints, so a
# killed chunk resumes. Launch detached via scripts/spawn_campaign.py so it
# survives this shell/agent session.
set -uo pipefail
cd "$(dirname "$0")/.."

DIR="${CAMPAIGN_DIR:-campaign}"
CHUNK="${CHUNK_SECONDS:-1800}"
mkdir -p "$DIR"

engine_loop() {
  local eng="$1" ports="$2" workers="$3"
  while [ ! -f "$DIR/DONE-$eng" ]; do
    uv run -- python -m scripts.parity.campaign run \
      --engine "$eng" --ports "$ports" --workers "$workers" \
      --max-seconds "$CHUNK" --dir "$DIR" >> "$DIR/$eng.log" 2>&1
    rc=$?
    echo "$(date '+%F %T') [$eng] chunk rc=$rc" >> "$DIR/supervisor.log"
    if [ "$rc" -eq 42 ]; then
      touch "$DIR/DONE-$eng"
      echo "$(date '+%F %T') [$eng] DONE" >> "$DIR/supervisor.log"
      break
    fi
    sleep 2
  done
}

echo "$(date '+%F %T') supervisor start (pid $$)" >> "$DIR/supervisor.log"
echo $$ > "$DIR/supervisor.pid"
# systematic no-ops once DONE-systematic exists (hard-complete), so covfuzz can
# take the whole 5-oracle pool when it is the only engine still running.
engine_loop systematic "55432,55433,55434" 6 &
engine_loop covfuzz    "55432,55433,55434,55435,55436" 10 &
wait
echo "$(date '+%F %T') all engines DONE" >> "$DIR/supervisor.log"
rm -f "$DIR/supervisor.pid"

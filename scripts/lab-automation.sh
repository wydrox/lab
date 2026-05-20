#!/usr/bin/env bash
set -euo pipefail

ROOT="${LAB_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
LAB_BIN="${LAB_BIN:-$HOME/.local/bin/lab-cli}"
YEAR="${LAB_YEAR:-2026}"
LOG_DIR="${LAB_LOG_DIR:-$HOME/Library/Logs/lab}"
LOCK_DIR="${LAB_LOCK_DIR:-$HOME/Library/Caches/lab/automation.lock}"
UPLOAD_JSON="$LOG_DIR/upload-$YEAR.json"
RECONCILE_JSON="$LOG_DIR/reconcile-$YEAR.json"
FINAL_RECONCILE_JSON="$LOG_DIR/reconcile-after-upload-$YEAR.json"
LOG_FILE="$LOG_DIR/automation.log"

mkdir -p "$LOG_DIR" "$(dirname "$LOCK_DIR")"

notify() {
  local title="$1"
  local message="$2"
  if command -v osascript >/dev/null 2>&1; then
    osascript -e "display notification \"${message//\"/\\\"}\" with title \"${title//\"/\\\"}\"" >/dev/null 2>&1 || true
  fi
}

log() {
  printf '[%s] %s\n' "$(date '+%Y-%m-%d %H:%M:%S')" "$*" | tee -a "$LOG_FILE"
}

if ! mkdir "$LOCK_DIR" 2>/dev/null; then
  log "Another LAB automation run is active; exiting."
  exit 0
fi
trap 'rmdir "$LOCK_DIR" 2>/dev/null || true' EXIT

if [[ ! -x "$LAB_BIN" ]]; then
  log "ERROR: LAB binary not executable: $LAB_BIN"
  notify "LAB automation failed" "LAB binary not executable"
  exit 1
fi

cd "$ROOT"

log "Start automation year=$YEAR root=$ROOT bin=$LAB_BIN"

run() {
  log "+ $*"
  set +e
  "$@" 2>&1 | tee -a "$LOG_FILE"
  local cmd_status=${PIPESTATUS[0]}
  set -e
  return "$cmd_status"
}

status=0
run "$LAB_BIN" sync --year "$YEAR" || status=$?
if [[ "$status" -eq 0 ]]; then
  run "$LAB_BIN" reconcile --year "$YEAR" --store --raw --output "$RECONCILE_JSON" || status=$?
fi
if [[ "$status" -eq 0 ]]; then
  run "$LAB_BIN" upload --year "$YEAR" --confirm --output "$UPLOAD_JSON" || status=$?
fi
if [[ "$status" -eq 0 ]]; then
  run "$LAB_BIN" sync --saldeo --year "$YEAR" || status=$?
fi
if [[ "$status" -eq 0 ]]; then
  run "$LAB_BIN" reconcile --year "$YEAR" --store --raw --output "$FINAL_RECONCILE_JSON" || status=$?
fi

if [[ "$status" -eq 0 ]]; then
  uploaded="unknown"
  failed="unknown"
  if command -v python3 >/dev/null 2>&1 && [[ -s "$UPLOAD_JSON" ]]; then
    uploaded="$(python3 - <<PY
import json
try:
    j=json.load(open('$UPLOAD_JSON'))
    print(j.get('summary',{}).get('uploaded_count','unknown'))
except Exception:
    print('unknown')
PY
)"
    failed="$(python3 - <<PY
import json
try:
    j=json.load(open('$UPLOAD_JSON'))
    print(j.get('summary',{}).get('failed_count','unknown'))
except Exception:
    print('unknown')
PY
)"
  fi
  log "Done. uploaded=$uploaded failed=$failed"
  notify "LAB automation done" "Uploaded: $uploaded, failed: $failed"
else
  log "FAILED with status=$status"
  notify "LAB automation failed" "Check $LOG_FILE"
fi

exit "$status"

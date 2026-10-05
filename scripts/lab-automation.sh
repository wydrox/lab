#!/usr/bin/env bash
set -euo pipefail

ROOT="${LAB_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
SCRIPT_NAME="$(basename "${BASH_SOURCE[0]}" .sh)"
LOG_DIR="${LAB_LOG_DIR:-$HOME/Library/Logs/lab}"
LOCK_DIR="${LAB_LOCK_DIR:-$HOME/Library/Caches/lab/automation.lock}"
LOG_FILE="$LOG_DIR/automation.log"

# Explicit LAB_BIN wins; else ~/.local/bin/lab, else the legacy ~/.local/bin/lab-cli.
resolve_lab_bin() {
  if [[ -n "${LAB_BIN:-}" ]]; then
    printf '%s\n' "$LAB_BIN"
  elif [[ ! -e "$HOME/.local/bin/lab" && -x "$HOME/.local/bin/lab-cli" ]]; then
    printf '%s\n' "$HOME/.local/bin/lab-cli"
  else
    printf '%s\n' "$HOME/.local/bin/lab"
  fi
}

notify() {
  local title="$1"
  local message="$2"
  if command -v osascript >/dev/null 2>&1; then
    # Text goes in as argv, so quotes/backslashes cannot break the AppleScript.
    osascript -e 'on run argv' \
      -e 'display notification (item 1 of argv) with title (item 2 of argv)' \
      -e 'end run' "$message" "$title" >/dev/null 2>&1 || true
  fi
}

log() {
  printf '[%s] %s\n' "$(date '+%Y-%m-%d %H:%M:%S')" "$*" | tee -a "$LOG_FILE"
}

# Years to process for a given date. Args: YEAR MONTH (e.g. 2027 01).
# January and February also cover the previous year (late December invoices).
automation_years() {
  local year="$1" month="$2"
  if (( 10#$month <= 2 )); then
    printf '%s %s\n' "$((year - 1))" "$year"
  else
    printf '%s\n' "$year"
  fi
}

# Prints "uploaded=N failed=M" from an `upload --output` JSON file.
upload_counts() {
  if ! command -v python3 >/dev/null 2>&1; then
    printf 'uploaded=unknown failed=unknown\n'
    return 0
  fi
  python3 - "$1" 2>/dev/null <<'PY' || printf 'uploaded=unknown failed=unknown\n'
import json
import sys


def count(summary, key):
    value = summary.get(key)
    return value if isinstance(value, int) and not isinstance(value, bool) else "unknown"


try:
    with open(sys.argv[1], encoding="utf-8") as fh:
        summary = json.load(fh).get("summary") or {}
    print("uploaded=%s failed=%s" % (count(summary, "uploaded_count"), count(summary, "failed_count")))
except Exception:
    print("uploaded=unknown failed=unknown")
PY
}

# True when PID is alive and still looks like this script (guards against
# the PID being reused by an unrelated process after a reboot).
lock_holder_alive() {
  local pid="$1" cmd
  kill -0 "$pid" 2>/dev/null || return 1
  if command -v ps >/dev/null 2>&1; then
    cmd="$(ps -p "$pid" -o command= 2>/dev/null || true)"
    if [[ -n "$cmd" && "$cmd" != *"$SCRIPT_NAME"* ]]; then
      return 1
    fi
  fi
  return 0
}

# Returns 0 when the lock was taken, 1 when another live run holds it,
# 2 when a stale lock could not be removed.
LOCK_HOLDER=""
acquire_lock() {
  local holder
  local _attempt
  for _attempt in 1 2; do
    if mkdir "$LOCK_DIR" 2>/dev/null; then
      printf '%s\n' "$$" > "$LOCK_DIR/pid.tmp"
      mv -f "$LOCK_DIR/pid.tmp" "$LOCK_DIR/pid"
      return 0
    fi
    holder="$(cat "$LOCK_DIR/pid" 2>/dev/null || true)"
    if [[ "$holder" =~ ^[0-9]+$ ]]; then
      if lock_holder_alive "$holder"; then
        LOCK_HOLDER="$holder"
        return 1
      fi
      log "Stale lock $LOCK_DIR: pid $holder is not running; taking it over."
    elif [[ -d "$LOCK_DIR" ]]; then
      # No pid yet: another run may be between mkdir and writing its pid.
      if [[ -n "$(find "$LOCK_DIR" -maxdepth 0 -mmin -1 2>/dev/null)" ]]; then
        LOCK_HOLDER="unknown"
        return 1
      fi
      log "Stale lock $LOCK_DIR without a pid file; taking it over."
    fi
    rm -f "$LOCK_DIR/pid" "$LOCK_DIR/pid.tmp"
    if ! rmdir "$LOCK_DIR" 2>/dev/null && [[ -e "$LOCK_DIR" ]]; then
      log "ERROR: cannot remove stale lock $LOCK_DIR"
      return 2
    fi
  done
  return 1
}

release_lock() {
  if [[ "$(cat "$LOCK_DIR/pid" 2>/dev/null || true)" == "$$" ]]; then
    rm -f "$LOCK_DIR/pid"
    rmdir "$LOCK_DIR" 2>/dev/null || true
  fi
  return 0
}

run() {
  log "+ $*"
  set +e
  "$@" 2>&1 | tee -a "$LOG_FILE"
  local cmd_status=${PIPESTATUS[0]}
  set -e
  return "$cmd_status"
}

# Runs one step unless an earlier step of the same year already failed.
YEAR_STATUS=0
FAILED_STEP=""
run_step() {
  local name="$1"
  shift
  if [[ "$YEAR_STATUS" -ne 0 ]]; then
    return 0
  fi
  local step_status=0
  run "$@" || step_status=$?
  if [[ "$step_status" -ne 0 ]]; then
    YEAR_STATUS="$step_status"
    FAILED_STEP="$name"
  fi
  return 0
}

# Full step chain for one year; sets YEAR_STATUS and YEAR_SUMMARY.
YEAR_SUMMARY=""
run_year() {
  local year="$1"
  local upload_json="$LOG_DIR/upload-$year.json"
  local upload_ran=0
  YEAR_STATUS=0
  FAILED_STEP=""

  log "Start year=$year"
  run_step "sync" "$LAB_BIN" sync --year "$year"
  run_step "reconcile" "$LAB_BIN" reconcile --year "$year" --store --raw --output "$LOG_DIR/reconcile-$year.json"
  run_step "repair" "$LAB_BIN" repair --year "$year" --confirm --output "$LOG_DIR/repair-$year.json"
  if [[ "$YEAR_STATUS" -eq 0 ]]; then
    # Never report an earlier run's upload result as this run's.
    if rm -f "$upload_json"; then
      upload_ran=1
    else
      log "ERROR: cannot remove previous upload output $upload_json"
      YEAR_STATUS=1
      FAILED_STEP="upload (stale output)"
    fi
  fi
  run_step "upload" "$LAB_BIN" upload --year "$year" --confirm --approve --output "$upload_json"
  run_step "sync --saldeo" "$LAB_BIN" sync --saldeo --year "$year"
  run_step "final reconcile" "$LAB_BIN" reconcile --year "$year" --store --raw --output "$LOG_DIR/reconcile-after-upload-$year.json"

  YEAR_SUMMARY="year=$year"
  if [[ "$YEAR_STATUS" -ne 0 ]]; then
    YEAR_SUMMARY+=" failed at $FAILED_STEP (status=$YEAR_STATUS)"
  fi
  if [[ "$upload_ran" -eq 1 ]]; then
    if [[ -s "$upload_json" ]]; then
      YEAR_SUMMARY+=" $(upload_counts "$upload_json")"
    else
      YEAR_SUMMARY+=" upload wrote no output"
    fi
  fi
  return 0
}

main() {
  mkdir -p "$LOG_DIR" "$(dirname "$LOCK_DIR")"

  local lock_status=0
  acquire_lock || lock_status=$?
  if [[ "$lock_status" -eq 1 ]]; then
    log "Another LAB automation run is active (pid $LOCK_HOLDER); exiting."
    exit 0
  elif [[ "$lock_status" -ne 0 ]]; then
    notify "LAB automation failed" "Cannot remove stale lock $LOCK_DIR"
    exit 1
  fi
  trap release_lock EXIT
  trap 'exit 129' HUP
  trap 'exit 130' INT
  trap 'exit 143' TERM

  LAB_BIN="$(resolve_lab_bin)"
  if [[ ! -x "$LAB_BIN" ]]; then
    log "ERROR: LAB binary not executable: $LAB_BIN"
    notify "LAB automation failed" "LAB binary not executable: $LAB_BIN"
    exit 1
  fi

  local years
  if [[ -n "${LAB_YEAR:-}" ]]; then
    if [[ ! "$LAB_YEAR" =~ ^[0-9]{4}$ ]]; then
      log "ERROR: LAB_YEAR must be a 4-digit year, got: $LAB_YEAR"
      notify "LAB automation failed" "Invalid LAB_YEAR: $LAB_YEAR"
      exit 1
    fi
    years="$LAB_YEAR"
  else
    years="$(automation_years "$(date +%Y)" "$(date +%m)")"
  fi

  cd "$ROOT"

  log "Start automation years=$years root=$ROOT bin=$LAB_BIN"

  local status=0 year summaries=""
  for year in $years; do
    run_year "$year"
    if [[ "$status" -eq 0 && "$YEAR_STATUS" -ne 0 ]]; then
      status="$YEAR_STATUS"
    fi
    summaries="${summaries:+$summaries; }$YEAR_SUMMARY"
  done

  if [[ "$status" -eq 0 ]]; then
    log "Done. $summaries"
    notify "LAB automation done" "$summaries"
  else
    log "FAILED with status=$status. $summaries"
    notify "LAB automation failed" "$summaries. Check $LOG_FILE"
  fi

  exit "$status"
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  main "$@"
fi

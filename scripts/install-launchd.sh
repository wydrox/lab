#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
LABEL="${LAB_LAUNCHD_LABEL:-com.rafalw.lab.automation}"
PLIST="$HOME/Library/LaunchAgents/$LABEL.plist"
# LAB_BIN and LAB_YEAR go into the plist only when set explicitly; otherwise
# lab-automation.sh resolves the binary and the year(s) on every run.
LAB_BIN_OVERRIDE="${LAB_BIN:-}"
YEAR_OVERRIDE="${LAB_YEAR:-}"
INTERVAL_SECONDS="${LAB_INTERVAL_SECONDS:-14400}" # every 4h by default
LOG_DIR="${LAB_LOG_DIR:-$HOME/Library/Logs/lab}"
SCRIPT="$ROOT/scripts/lab-automation.sh"
# launchd starts agents with PATH=/usr/bin:/bin:/usr/sbin:/sbin; the binary
# needs node/npm for the Saldeo re-login and Homebrew tools (poppler).
BASE_PATH="/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin"

# Explicit LAB_BIN wins; else ~/.local/bin/lab, else the legacy ~/.local/bin/lab-cli.
resolve_lab_bin() {
  if [[ -n "${LAB_BIN_OVERRIDE:-}" ]]; then
    printf '%s\n' "$LAB_BIN_OVERRIDE"
  elif [[ ! -e "$HOME/.local/bin/lab" && -x "$HOME/.local/bin/lab-cli" ]]; then
    printf '%s\n' "$HOME/.local/bin/lab-cli"
  else
    printf '%s\n' "$HOME/.local/bin/lab"
  fi
}

# Directories where node and npm resolve now, then BASE_PATH, without duplicates.
build_launchd_path() {
  local result="" tool found dir
  for tool in node npm; do
    found="$(command -v "$tool" 2>/dev/null || true)"
    # Only real files (not aliases/functions) and dirs that are safe in PATH.
    [[ "$found" == /* ]] || continue
    dir="$(dirname "$found")"
    [[ "$dir" != *:* ]] || continue
    case ":$result:" in
      *":$dir:"*) ;;
      *) result="${result:+$result:}$dir" ;;
    esac
  done
  local IFS=:
  for dir in $BASE_PATH; do
    case ":$result:" in
      *":$dir:"*) ;;
      *) result="${result:+$result:}$dir" ;;
    esac
  done
  printf '%s\n' "$result"
}

xml_escape() {
  local s="$1"
  # Unquoted assignment + quoted replacement is the only form that works in
  # both bash 3.2 (/bin/bash) and bash >= 5.2 (patsub_replacement).
  s=${s//&/'&amp;'}
  s=${s//</'&lt;'}
  s=${s//>/'&gt;'}
  printf '%s' "$s"
}

env_entry() {
  printf '    <key>%s</key>\n    <string>%s</string>\n' "$1" "$(xml_escape "$2")"
}

render_env() {
  env_entry PATH "$1"
  env_entry LAB_ROOT "$ROOT"
  if [[ -n "$LAB_BIN_OVERRIDE" ]]; then
    env_entry LAB_BIN "$LAB_BIN_OVERRIDE"
  fi
  if [[ -n "$YEAR_OVERRIDE" ]]; then
    env_entry LAB_YEAR "$YEAR_OVERRIDE"
  fi
  env_entry LAB_LOG_DIR "$LOG_DIR"
}

# Writes the plist to stdout. Args: launchd PATH.
render_plist() {
  local launchd_path="$1"
  cat <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>$(xml_escape "$LABEL")</string>

  <key>ProgramArguments</key>
  <array>
    <string>$(xml_escape "$SCRIPT")</string>
  </array>

  <key>EnvironmentVariables</key>
  <dict>
$(render_env "$launchd_path")
  </dict>

  <key>WorkingDirectory</key>
  <string>$(xml_escape "$ROOT")</string>

  <key>RunAtLoad</key>
  <true/>

  <key>StartInterval</key>
  <integer>$INTERVAL_SECONDS</integer>

  <key>StandardOutPath</key>
  <string>$(xml_escape "$LOG_DIR/launchd.out.log")</string>
  <key>StandardErrorPath</key>
  <string>$(xml_escape "$LOG_DIR/launchd.err.log")</string>
</dict>
</plist>
PLIST
}

main() {
  local lab_bin launchd_path
  lab_bin="$(resolve_lab_bin)"

  if [[ ! -x "$SCRIPT" ]]; then
    echo "ERROR: automation script is not executable: $SCRIPT" >&2
    exit 1
  fi
  if [[ ! -x "$lab_bin" ]]; then
    echo "ERROR: LAB binary is not executable: $lab_bin" >&2
    exit 1
  fi
  if [[ -n "$YEAR_OVERRIDE" && ! "$YEAR_OVERRIDE" =~ ^[0-9]{4}$ ]]; then
    echo "ERROR: LAB_YEAR must be a 4-digit year, got: $YEAR_OVERRIDE" >&2
    exit 1
  fi
  if [[ ! "$INTERVAL_SECONDS" =~ ^[0-9]+$ ]]; then
    echo "ERROR: LAB_INTERVAL_SECONDS must be a number of seconds, got: $INTERVAL_SECONDS" >&2
    exit 1
  fi
  if ! command -v node >/dev/null 2>&1; then
    echo "WARNING: node not found in PATH; when the Saldeo session expires, automatic re-login will fail until you log in by hand." >&2
  fi
  if ! command -v npm >/dev/null 2>&1; then
    echo "WARNING: npm not found in PATH; Playwright for the Saldeo re-login cannot be installed automatically." >&2
  fi
  launchd_path="$(build_launchd_path)"

  mkdir -p "$HOME/Library/LaunchAgents" "$LOG_DIR"
  render_plist "$launchd_path" > "$PLIST"
  if command -v plutil >/dev/null 2>&1; then
    plutil -lint -s "$PLIST"
  fi

  launchctl bootout "gui/$(id -u)" "$PLIST" >/dev/null 2>&1 || true
  launchctl bootstrap "gui/$(id -u)" "$PLIST"
  launchctl enable "gui/$(id -u)/$LABEL"
  launchctl kickstart -k "gui/$(id -u)/$LABEL"

  echo "Installed and started: $LABEL"
  echo "Plist: $PLIST"
  echo "Binary: $lab_bin${LAB_BIN_OVERRIDE:+ (fixed by LAB_BIN)}"
  if [[ -n "$YEAR_OVERRIDE" ]]; then
    echo "Year: $YEAR_OVERRIDE (fixed by LAB_YEAR)"
  else
    echo "Year: current year at run time (Jan-Feb: previous year, then current)"
  fi
  echo "PATH: $launchd_path"
  echo "Logs: $LOG_DIR/automation.log"
  echo "Interval seconds: $INTERVAL_SECONDS"
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  main "$@"
fi

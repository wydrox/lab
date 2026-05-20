#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
LABEL="${LAB_LAUNCHD_LABEL:-com.rafalw.lab.automation}"
PLIST="$HOME/Library/LaunchAgents/$LABEL.plist"
LAB_BIN="${LAB_BIN:-$HOME/.local/bin/lab-cli}"
YEAR="${LAB_YEAR:-2026}"
INTERVAL_SECONDS="${LAB_INTERVAL_SECONDS:-14400}" # every 4h by default
LOG_DIR="${LAB_LOG_DIR:-$HOME/Library/Logs/lab}"
SCRIPT="$ROOT/scripts/lab-automation.sh"

if [[ ! -x "$SCRIPT" ]]; then
  echo "ERROR: automation script is not executable: $SCRIPT" >&2
  exit 1
fi
if [[ ! -x "$LAB_BIN" ]]; then
  echo "ERROR: LAB binary is not executable: $LAB_BIN" >&2
  exit 1
fi

mkdir -p "$HOME/Library/LaunchAgents" "$LOG_DIR"

cat > "$PLIST" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>$LABEL</string>

  <key>ProgramArguments</key>
  <array>
    <string>$SCRIPT</string>
  </array>

  <key>EnvironmentVariables</key>
  <dict>
    <key>LAB_ROOT</key>
    <string>$ROOT</string>
    <key>LAB_BIN</key>
    <string>$LAB_BIN</string>
    <key>LAB_YEAR</key>
    <string>$YEAR</string>
    <key>LAB_LOG_DIR</key>
    <string>$LOG_DIR</string>
  </dict>

  <key>WorkingDirectory</key>
  <string>$ROOT</string>

  <key>RunAtLoad</key>
  <true/>

  <key>StartInterval</key>
  <integer>$INTERVAL_SECONDS</integer>

  <key>StandardOutPath</key>
  <string>$LOG_DIR/launchd.out.log</string>
  <key>StandardErrorPath</key>
  <string>$LOG_DIR/launchd.err.log</string>
</dict>
</plist>
PLIST

launchctl bootout "gui/$(id -u)" "$PLIST" >/dev/null 2>&1 || true
launchctl bootstrap "gui/$(id -u)" "$PLIST"
launchctl enable "gui/$(id -u)/$LABEL"
launchctl kickstart -k "gui/$(id -u)/$LABEL"

echo "Installed and started: $LABEL"
echo "Plist: $PLIST"
echo "Logs: $LOG_DIR/automation.log"
echo "Interval seconds: $INTERVAL_SECONDS"

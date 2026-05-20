#!/usr/bin/env bash
set -euo pipefail

LABEL="${LAB_LAUNCHD_LABEL:-com.rafalw.lab.automation}"
PLIST="$HOME/Library/LaunchAgents/$LABEL.plist"

launchctl bootout "gui/$(id -u)" "$PLIST" >/dev/null 2>&1 || true
rm -f "$PLIST"
echo "Uninstalled: $LABEL"

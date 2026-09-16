#!/usr/bin/env bash
set -euo pipefail

OUT="${1:-${SALDEO_STORAGE_STATE:-$HOME/.config/lab/saldeo-storage-state.json}}"
URL="${SALDEO_URL:-https://saldeo.brainshare.pl/}"
HELIUM_EXECUTABLE="${HELIUM_EXECUTABLE:-/Applications/Helium.app/Contents/MacOS/Helium}"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
LOGIN_JS="$SCRIPT_DIR/saldeo-login.js"

mkdir -p "$(dirname "$OUT")"

cat <<EOF
Saldeo auth
===========
Jeśli w Keychain lub środowisku są SALDEO_USERNAME i SALDEO_PASSWORD,
skrypt wypełni formularz sam. W przeciwnym razie otworzy Helium
i poczeka, aż sesja będzie ważna.
Zapis: $OUT
EOF

if ! command -v npx >/dev/null 2>&1; then
  echo "ERROR: brak npx. Zainstaluj Node.js." >&2
  exit 1
fi

if [[ ! -x "$HELIUM_EXECUTABLE" ]]; then
  echo "ERROR: nie znalazłem Helium executable: $HELIUM_EXECUTABLE" >&2
  exit 1
fi

LOGIN_FILE=""
cleanup() {
  if [[ -n "$LOGIN_FILE" ]]; then
    rm -f "$LOGIN_FILE"
  fi
}
trap cleanup EXIT

if [[ -n "${SALDEO_USERNAME:-}" && -n "${SALDEO_PASSWORD:-}" ]]; then
  LOGIN_FILE="$(mktemp -t lab-saldeo-login.XXXXXX)"
  umask 077
  python3 -c 'import json,os,sys; json.dump({"username":os.environ["SALDEO_USERNAME"],"password":os.environ["SALDEO_PASSWORD"]}, open(sys.argv[1],"w"))' "$LOGIN_FILE"
  chmod 600 "$LOGIN_FILE"
  export LAB_SALDEO_LOGIN_FILE="$LOGIN_FILE"
fi

LAB_SALDEO_STORAGE_STATE="$OUT" \
SALDEO_URL="$URL" \
HELIUM_EXECUTABLE="$HELIUM_EXECUTABLE" \
npx --yes -p playwright node "$LOGIN_JS"

if [[ ! -s "$OUT" ]]; then
  echo "ERROR: storage state nie został zapisany: $OUT" >&2
  exit 1
fi

chmod 600 "$OUT" 2>/dev/null || true
echo "✓ Zapisano Saldeo auth: $OUT"
echo "Sprawdź: lab onboard --check"

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
Login i hasło skrypt bierze z pliku wskazanego przez LAB_SALDEO_LOGIN_FILE
(tak przekazuje je \`lab onboard\`) albo ze zmiennych środowiskowych
SALDEO_USERNAME i SALDEO_PASSWORD; sam nie czyta Keychain ani .env.
Bez danych logowania otworzy Helium i poczeka, aż zalogujesz się w oknie.
Zapis: $OUT
EOF

if ! command -v node >/dev/null 2>&1 || ! command -v npm >/dev/null 2>&1; then
  echo "ERROR: brak node/npm. Zainstaluj Node.js." >&2
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

PREFIX="${LAB_PLAYWRIGHT_PREFIX:-$HOME/.config/lab/playwright}"
# Ta sama wersja co PLAYWRIGHT_VERSION w src/onboard.rs; LAB_PLAYWRIGHT_VERSION nadpisuje.
PLAYWRIGHT_VERSION="${LAB_PLAYWRIGHT_VERSION:-1.63.0}"
# Zainstalowana wersja zostaje, nawet jeśli inna niż przypięta.
if [[ ! -d "$PREFIX/node_modules/playwright" ]]; then
  if [[ ${#PLAYWRIGHT_VERSION} -gt 64 || ! "$PLAYWRIGHT_VERSION" =~ ^[A-Za-z0-9][A-Za-z0-9.-]*$ ]]; then
    echo "ERROR: LAB_PLAYWRIGHT_VERSION: oczekuję wersji albo dist-tagu (litery, cyfry, '.', '-'): $PLAYWRIGHT_VERSION" >&2
    exit 1
  fi
  echo "Instaluję Playwright $PLAYWRIGHT_VERSION w $PREFIX (bez przeglądarki Playwright)..."
  PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD=1 npm install --prefix "$PREFIX" --ignore-scripts --no-fund --no-audit "playwright@$PLAYWRIGHT_VERSION"
fi
LAB_SALDEO_STORAGE_STATE="$OUT" \
SALDEO_URL="$URL" \
HELIUM_EXECUTABLE="$HELIUM_EXECUTABLE" \
NODE_PATH="$PREFIX/node_modules" \
node "$LOGIN_JS"

if [[ ! -s "$OUT" ]]; then
  echo "ERROR: storage state nie został zapisany: $OUT" >&2
  exit 1
fi

chmod 600 "$OUT" 2>/dev/null || true
echo "✓ Zapisano Saldeo auth: $OUT"
echo "Sprawdź: lab onboard --check"

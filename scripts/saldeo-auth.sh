#!/usr/bin/env bash
set -euo pipefail

OUT="${1:-${SALDEO_STORAGE_STATE:-$HOME/.config/lab/saldeo-storage-state.json}}"
URL="${SALDEO_URL:-https://saldeo.brainshare.pl/}"
HELIUM_EXECUTABLE="${HELIUM_EXECUTABLE:-/Applications/Helium.app/Contents/MacOS/Helium}"

mkdir -p "$(dirname "$OUT")"

cat <<EOF
Saldeo auth capture
===================
1. Otworzy się Helium Browser na: $URL
2. Zaloguj się do Saldeo.
3. Po udanym logowaniu wróć do terminala i naciśnij Enter.
4. Storage state zostanie zapisany tutaj:
   $OUT

Helium executable:
   $HELIUM_EXECUTABLE

Wymagane: Node.js + npx. Playwright zostanie pobrany przez npx, jeśli go nie ma.
EOF

if ! command -v npx >/dev/null 2>&1; then
  echo "ERROR: brak npx. Zainstaluj Node.js albo Playwright." >&2
  exit 1
fi

if [[ ! -x "$HELIUM_EXECUTABLE" ]]; then
  echo "ERROR: nie znalazłem Helium executable: $HELIUM_EXECUTABLE" >&2
  echo "Ustaw HELIUM_EXECUTABLE=/ścieżka/do/Helium, np.:" >&2
  echo "  HELIUM_EXECUTABLE='/Applications/Helium.app/Contents/MacOS/Helium' ./scripts/saldeo-auth.sh" >&2
  exit 1
fi

TMP_JS="$(mktemp -t lab-saldeo-auth.XXXXXX.js)"
trap 'rm -f "$TMP_JS"' EXIT
cat > "$TMP_JS" <<'JS'
const { chromium } = require('playwright');
const fs = require('fs');
const os = require('os');
const path = require('path');
const readline = require('readline');

async function waitForEnter() {
  const rl = readline.createInterface({ input: process.stdin, output: process.stdout });
  await new Promise(resolve => rl.question('\nPo zalogowaniu naciśnij Enter, żeby zapisać auth... ', resolve));
  rl.close();
}

(async () => {
  const out = process.env.LAB_SALDEO_STORAGE_STATE;
  const url = process.env.SALDEO_URL || 'https://saldeo.brainshare.pl/';
  const executablePath = process.env.HELIUM_EXECUTABLE;
  const userDataDir = path.join(os.homedir(), '.config', 'lab', 'helium-profile');
  fs.mkdirSync(userDataDir, { recursive: true });
  const context = await chromium.launchPersistentContext(userDataDir, {
    executablePath,
    headless: false,
    viewport: { width: 1400, height: 1000 },
  });
  const page = context.pages()[0] || await context.newPage();
  await page.goto(url, { waitUntil: 'domcontentloaded' });
  await waitForEnter();
  await context.storageState({ path: out });
  await context.close();
})().catch(err => {
  console.error(err && err.stack ? err.stack : err);
  process.exit(1);
});
JS

LAB_SALDEO_STORAGE_STATE="$OUT" \
SALDEO_URL="$URL" \
HELIUM_EXECUTABLE="$HELIUM_EXECUTABLE" \
npx --yes -p playwright node -e "$(cat "$TMP_JS")"

if [[ ! -s "$OUT" ]]; then
  echo "ERROR: storage state nie został zapisany: $OUT" >&2
  exit 1
fi

chmod 600 "$OUT" 2>/dev/null || true

echo
echo "✓ Zapisano Saldeo auth: $OUT"
echo "Sprawdź: lab onboard --check"

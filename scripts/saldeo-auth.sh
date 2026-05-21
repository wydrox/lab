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
3. Skrypt sam wykryje poprawne cookies i zapisze auth (bez naciskania Enter).
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

function sleep(ms) {
  return new Promise(resolve => setTimeout(resolve, ms));
}

function cookieHeader(cookies) {
  return cookies.map(cookie => `${cookie.name}=${cookie.value}`).join('; ');
}

function xsrfToken(cookies) {
  const cookie = cookies.find(cookie => cookie.name === 'X-SALDEO-XSRF-C-TOKEN');
  return cookie && cookie.value;
}

function authCheckBody() {
  return {
    pagination: { pageNumber: 0, pageSize: 1, totalCount: 0,
      columnSorted: { sortColumn: 'DOCUMENT_CREATE_DATE', sortDirection: 'ASC' } },
    filter: { period: { partOfYear: 1, year: new Date().getFullYear(), selectionType: 'selectedMonth' },
      duplicatesEnable: false, duplicates: false, splitPayment: false,
      types: [], contractors: [], stages: [], categories: [], registers: [],
      tags: [], assignUsers: [], addedBy: [], added: [],
      paymentStatuses: [], accountingPaymentTypes: [],
      searchQuery: '', selectKsefDocumentsYesCheckbox: false,
      selectKsefDocumentsNoCheckbox: false, ksefNumber: '',
      ksefMiniWorkflowStatus: null, ksefBoId: null,
      dimensionReportDocumentIds: [], dimensions: null }
  };
}

async function storageAuthenticated(context) {
  const state = await context.storageState();
  const cookies = state.cookies || [];
  const xsrf = xsrfToken(cookies);
  if (!xsrf) return false;
  try {
    const response = await context.request.post('https://saldeo.brainshare.pl/rest/client/document/list/search', {
      headers: {
        Cookie: cookieHeader(cookies),
        'X-SALDEO-XSRF-H-TOKEN': xsrf,
        saldeoApp: 'angularApp',
        timeout: '60000',
      },
      data: authCheckBody(),
    });
    return response.ok();
  } catch (_) {
    return false;
  }
}

(async () => {
  const out = process.env.LAB_SALDEO_STORAGE_STATE;
  const url = process.env.SALDEO_URL || 'https://saldeo.brainshare.pl/';
  const executablePath = process.env.HELIUM_EXECUTABLE;
  const timeoutMs = Number.parseInt(process.env.SALDEO_AUTH_TIMEOUT_MS || '180000', 10);
  const userDataDir = path.join(os.homedir(), '.config', 'lab', 'helium-profile');
  fs.mkdirSync(userDataDir, { recursive: true });
  const context = await chromium.launchPersistentContext(userDataDir, {
    executablePath,
    headless: false,
    viewport: { width: 1400, height: 1000 },
  });
  let closed = false;
  context.once('close', () => { closed = true; });
  const page = context.pages()[0] || await context.newPage();
  await page.goto(url, { waitUntil: 'domcontentloaded' });
  console.error(`Czekam na poprawną sesję Saldeo (timeout ${Math.round(timeoutMs / 1000)}s)...`);

  const deadline = Date.now() + timeoutMs;
  let authenticated = false;
  while (!closed && Date.now() < deadline) {
    await context.storageState({ path: out }).catch(() => {});
    if (await storageAuthenticated(context)) {
      authenticated = true;
      break;
    }
    await sleep(2000);
  }

  if (!closed) {
    await context.storageState({ path: out });
    await context.close().catch(() => {});
  }
  if (!authenticated) {
    console.error(`Saldeo auth timeout after ${Math.round(timeoutMs / 1000)}s`);
    process.exit(2);
  }
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

# LAB — Lazy Accounting Buddy

Małe CLI w Rust do uzgadniania faktur z eksportu KSeF z fakturami znalezionymi w Gmailu. Program jest „agent-friendly”: komendy są nieinteraktywne, wejścia/wyjścia mogą być plikami, a raport domyślnie jest JSON.

## Co robi MVP

- skanuje faktury z katalogu/plików: `.xml`, `.json`, `.txt`, `.eml`, `.pdf`,
- z XML KSeF wyciąga m.in. `P_1`, `P_2`, `P_15`, `KodWaluty`, NIP-y z `Podmiot1`/`Podmiot2`,
- z maili/tekstów/PDF-ów próbuje wyciągnąć numer faktury, NIP-y, kontrahentów, datę wystawienia/sprzedaży/płatności, kwoty i walutę,
- uzgadnia rekordy po numerze faktury, NIP-ach, kwocie brutto, dacie i walucie,
- zapisuje rekordy i przebiegi uzgodnień w dedykowanej bazie SQLite,
- może pobrać załączniki z Gmail API, jeśli podasz OAuth bearer token.

To nie finalizuje sesji KSeF przez oficjalne API — na start najbezpieczniejszy przepływ to eksport XML/JSON z KSeF + pobranie załączników z Gmaila.

## Instalacja / build

```bash
git clone https://github.com/<owner>/lab.git
cd lab
cargo build --release
```

Binarka będzie w:

```bash
./target/release/lab-cli
```

PDF-y są parsowane przez `pdftotext` z pakietu poppler, z fallbackiem do Python `pypdf`:

```bash
brew install poppler
python3 -m pip install pypdf
```

## Szybkie użycie dla agentów AI

### 1. Skan KSeF do JSONL

```bash
lab-cli scan \
  --source ksef \
  --input ./data/ksef \
  --format jsonl \
  --output ./out/ksef.jsonl
```

### 2. Skan faktur z maila do JSONL

```bash
lab-cli scan \
  --source mail \
  --input ./data/mail \
  --format jsonl \
  --output ./out/mail.jsonl
```

### 3. Uzgodnienie

```bash
lab-cli reconcile \
  --ksef ./out/ksef.jsonl \
  --mail ./out/mail.jsonl \
  --output ./out/report.json \
  --csv ./out/matches.csv
```

Statusy:

- `matched` — pewne dopasowanie, domyślnie score `>= 70`,
- `needs_review` — prawdopodobne dopasowanie, domyślnie score `45..69`,
- `unmatched_ksef` / `unmatched_mail` — brak pary.

## SQLite

Domyślna baza to `./lab.sqlite`; można ją zmienić globalną flagą `--db`.

```bash
lab-cli --db ./data/reconcile.sqlite db init
lab-cli --db ./data/reconcile.sqlite scan --source ksef --input ./data/ksef --store --output ./out/ksef.jsonl
lab-cli --db ./data/reconcile.sqlite scan --source mail --input ./data/mail --store --output ./out/mail.jsonl
lab-cli --db ./data/reconcile.sqlite reconcile-db --output ./out/report.json --csv ./out/matches.csv
lab-cli --db ./data/reconcile.sqlite db stats
lab-cli --db ./data/reconcile.sqlite db tri-runs --limit 10
```

W bazie są tabele `invoices`, `reconcile_runs`, `invoice_matches` oraz temporalne `tri_reconcile_runs` / `tri_reconcile_rows` do śledzenia diffów między przebiegami. Sekretów OAuth/KSeF nie zapisujemy w SQLite — tylko dane faktur i wyniki uzgodnień.

## Pobieranie załączników z Gmaila

Najwygodniej użyć Google OAuth Desktop Client JSON i komendy `gmail-auth`. Token jest zapisywany poza repo, domyślnie w `~/.config/lab/gmail_token.json`.

```bash
lab-cli gmail-auth \
  --client-secret /path/to/google-oauth-desktop-client.json
```

Potem pobieranie załączników:

```bash
lab-cli gmail-fetch \
  --client-secret /path/to/google-oauth-desktop-client.json \
  --query 'has:attachment (filename:pdf OR filename:xml) newer_than:365d' \
  --out ./data/mail \
  --max 200
```

Jeśli chcesz jednorazowo pominąć token-file, możesz nadal użyć env var:

```bash
export GMAIL_ACCESS_TOKEN='<access-token>'
```

Program zapisze pełny JSON wiadomości oraz załączniki `.pdf`, `.xml`, `.json`, `.txt`.

## KSeF sync

LAB synchronizuje lokalny eksport KSeF/rekordy do własnego formatu JSON + JSONL i opcjonalnie SQLite:

```bash
lab-cli --db ./data/full-2026.sqlite ksef-sync \
  --year 2026 \
  --input ./data/ksef-export \
  --out ./data/ksef-2026 \
  --store
```

To nadal nie zapisuje tokenów ani sekretów KSeF. Oficjalny fetch API może zostać dodany później jako osobny connector.

## Cykliczny flow 1. i 14. dnia miesiąca

Najprostsza komenda do regularnego przebiegu ProductMesh: Gmail/PDF → skan → KSeF sync → Saldeo → tri-reconcile → temporal diff → raport braków dla księgowej.

```bash
lab-cli --db ./data/full-2026.sqlite cycle run \
  --year 2026 \
  --ksef ./data/ksef-productmesh-2026/ksef_records.jsonl \
  --store \
  --copy-missing-non-ksef
```

Jeżeli chcesz tylko odświeżyć raport na już pobranych danych:

```bash
lab-cli cycle run \
  --year 2026 \
  --ksef ./data/ksef-productmesh-2026/ksef_records.jsonl \
  --skip-gmail-fetch \
  --skip-saldeo-fetch
```

Raport faktur z Gmaila, których nie ma w Saldeo:

```bash
lab-cli cycle missing \
  --year 2026 \
  --tri-report ./out/tri-reconcile-2026.json \
  --output ./out/accountant-missing-2026.json \
  --csv ./out/accountant-missing-2026.csv \
  --copy-non-ksef
```

Podpowiedź do crona/launchd dla uruchamiania 1. i 14. dnia miesiąca:

```bash
lab-cli cycle schedule --year 2026 --ksef ./data/ksef-productmesh-2026/ksef_records.jsonl
```

## MCP dla agentów

CLI ma serwer MCP po stdio:

```bash
lab-cli --db ./data/full-2026.sqlite mcp
```

Przykładowa konfiguracja jest w `mcp/lab-mcp.example.json`. Skopiuj ją do konfiguracji swojego klienta MCP i podmień ścieżki. Dostępne narzędzia MCP:

- `cycle_run`
- `cycle_missing`
- `ksef_sync`
- `saldeo_fetch`
- `tri_reconcile`
- `db_stats`
- `tri_runs`

## SaldeoSMART i porównanie 3-stronne

Saldeo używa zapisanej sesji webowej Playwright (`~/.config/lab/saldeo-storage-state.json`). Sekrety trzymaj w macOS Keychain; nie zapisuj ich w repo.

```bash
lab-cli --db ./data/full-2026.sqlite saldeo-fetch \
  --year 2026 \
  --out ./data/saldeo-2026 \
  --store

lab-cli --db ./data/full-2026.sqlite tri-reconcile \
  --mail ./out/mail-all-pdf-2026-productmesh-candidates.jsonl \
  --ksef ./data/ksef-2026/records.jsonl \
  --saldeo ./data/saldeo-2026/records.jsonl \
  --review-score 70 \
  --output ./out/tri-reconcile-2026.json \
  --csv ./out/tri-reconcile-2026.csv \
  --store \
  --year 2026
```

Statusy raportu obejmują m.in. `in_all_three`, `gmail_only`, `ksef_saldeo_missing_gmail`, `saldeo_only`.

## Diagnostyka

```bash
lab-cli doctor
```

Zwraca JSON z informacją, czy widzi `GMAIL_ACCESS_TOKEN` i `pdftotext`.

## Model dopasowania

Maksymalnie 100 punktów:

- numer faktury exact: `+45`, partial: `+25`,
- dowolny zgodny NIP: `+20`, seller NIP w tej samej pozycji: `+5`,
- kwota brutto exact: `+20`, prawie exact: `+17`,
- data wystawienia exact: `+10`, w zakresie 7 dni: `+4`,
- waluta: `+5`.

Progi można zmienić:

```bash
lab-cli reconcile --ksef ksef.jsonl --mail mail.jsonl --match-score 80 --review-score 50
```

## KSeF / sekrety lokalne

Certyfikat, klucz, hasło i token trzymaj poza repo, np. jako env vars:

```bash
export KSEF_CERT_PATH='/ścieżka/do/certyfikatu.crt'
export KSEF_KEY_PATH='/ścieżka/do/klucza.key'
export KSEF_CERT_PASSWORD='...'
export KSEF_TOKEN='...'
```

Aktualny MVP jeszcze nie wywołuje oficjalnego API KSeF — używa eksportów XML/JSON. Te env vars są przygotowaniem pod kolejny krok integracji.

## Uwagi bezpieczeństwa

- Nie zapisuj tokenów, haseł ani kluczy w repo — używaj env var albo systemowego keychain.
- Raporty mogą zawierać NIP-y, numery faktur i ścieżki plików; traktuj `out/` jako dane wrażliwe.
- Ten MVP nie księguje i nie wysyła nic do KSeF; tylko czyta pliki/Gmail i porównuje dane.

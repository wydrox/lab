# LAB — Lazy Accounting Buddy

Małe CLI w Rust do uzgadniania faktur z KSeF, Gmaila i SaldeoSMART. Program jest „agent-friendly”: komendy są nieinteraktywne, wejścia/wyjścia mogą być plikami, a raport domyślnie jest JSON.

## Cztery główne komendy

```bash
lab-cli sync        # synchronizuje dane z Gmaila, KSeF i/lub Saldeo
lab-cli reconcile   # porównuje wszystkie trzy źródła
lab-cli upload      # wysyła brakujące faktury do Saldeo
lab-cli gmail-auth  # jednorazowa autoryzacja Gmail OAuth
```

## Instalacja / build

```bash
git clone https://github.com/<owner>/lab.git
cd lab
cargo build --release
```

Binarka: `./target/release/lab-cli`

PDF-y są parsowane przez `pdftotext` (poppler), z fallbackiem do Python `pypdf`:

```bash
brew install poppler
python3 -m pip install pypdf
```

## Szybkie użycie

### 1. Jednorazowa autoryzacja Gmaila

```bash
lab-cli gmail-auth --client-secret /path/to/google-oauth-desktop-client.json
```

Token zapisywany jest w `~/.config/lab/gmail_token.json`.

### 2. Synchronizacja danych

Wszystkie trzy źródła naraz:

```bash
lab-cli --db ./data/full-2026.sqlite sync --store
```

Tylko KSeF:

```bash
lab-cli sync --ksef --ksef-input ./data/ksef-export --store
```

Tylko Gmail/PDF (pobiera załączniki, parsuje PDF-y, odrzuca nie-faktury, zapisuje do DB):

```bash
lab-cli sync --mail --store
```

Tylko Saldeo:

```bash
lab-cli sync --saldeo --store
```

### 3. Uzgodnienie (tri-reconcile)

Porównanie trzech źródeł i zapis temporalnego snapshotu:

```bash
lab-cli --db ./data/full-2026.sqlite reconcile \
  --mail ./out/mail-candidates.jsonl \
  --ksef ./data/ksef-2026/records.jsonl \
  --saldeo ./data/saldeo-2026/records.jsonl \
  --output ./out/tri-reconcile-2026.json \
  --csv ./out/tri-reconcile-2026.csv \
  --store --year 2026
```

Pokaż ostatni raport z bazy (bez ponownego liczenia):

```bash
lab-cli reconcile --status --year 2026
```

Statusy raportu: `in_all_three`, `gmail_ksef_missing_saldeo`, `gmail_saldeo_missing_ksef`, `gmail_only`, `ksef_saldeo_missing_gmail`, `ksef_only`, `saldeo_only`.

### 4. Upload brakujących do Saldeo

```bash
lab-cli upload \
  --tri-report ./out/tri-reconcile-2026.json \
  --output ./out/upload-result.json
```

Albo z jawnych ścieżek (bez raportu tri-reconcile):

```bash
lab-cli upload \
  --mail ./out/mail-candidates.jsonl \
  --ksef ./data/ksef-2026/records.jsonl \
  --saldeo ./data/saldeo-2026/records.jsonl
```

Upload używa zweryfikowanego flow Saldeo: `generate-urls-for-upload` → `PUT` signed URL → `confirm`.

## Pozostałe komendy

- `scan` — niskopoziomowe skanowanie plików do znormalizowanych rekordów
- `gmail-fetch` — ręczne pobieranie załączników z Gmaila
- `db` — operacje na SQLite (`init`, `stats`, `list`, `tri-runs`)
- `doctor` — diagnostyka środowiska
- `mcp` — serwer MCP dla agentów AI

## SQLite

Domyślna baza to `./lab.sqlite`; można ją zmienić globalną flagą `--db`.

```bash
lab-cli --db ./data/reconcile.sqlite db init
lab-cli --db ./data/reconcile.sqlite db stats
lab-cli --db ./data/reconcile.sqlite db tri-runs --limit 10
```

W bazie są tabele `invoices`, `reconcile_runs`, `invoice_matches` oraz temporalne `tri_reconcile_runs` / `tri_reconcile_rows` do śledzenia diffów między przebiegami. Sekretów OAuth/KSeF nie zapisujemy w SQLite — tylko dane faktur i wyniki uzgodnień.

## MCP dla agentów

```bash
lab-cli --db ./data/full-2026.sqlite mcp
```

Konfiguracja w `mcp/lab-mcp.example.json`. Dostępne narzędzia MCP:

- `cycle_run`, `cycle_missing`
- `ksef_sync`, `saldeo_sync`, `saldeo_fetch`
- `tri_reconcile`
- `db_stats`, `tri_runs`

## Model dopasowania

Maksymalnie 100 punktów:

- numer faktury exact: `+45`, partial: `+25`,
- dowolny zgodny NIP: `+20`, seller NIP w tej samej pozycji: `+5`,
- kwota brutto exact: `+20`, prawie exact: `+17`,
- data wystawienia exact: `+10`, w zakresie 7 dni: `+4`,
- waluta: `+5`.

Próg review można zmienić:

```bash
lab-cli reconcile --review-score 50 ...
```

## Diagnostyka

```bash
lab-cli doctor
```

Zwraca JSON z informacją, czy widzi `GMAIL_ACCESS_TOKEN` i `pdftotext`.

## KSeF / sekrety lokalne

Certyfikat, klucz, hasło i token trzymaj poza repo, np. jako env vars:

```bash
export KSEF_CERT_PATH='/ścieżka/do/certyfikatu.crt'
export KSEF_KEY_PATH='/ścieżka/do/klucza.key'
export KSEF_CERT_PASSWORD='...'
export KSEF_TOKEN='...'
```

Aktualny MVP jeszcze nie wywołuje oficjalnego API KSeF — używa eksportów XML/JSON. Te env vars są przygotowaniem pod kolejny krok integracji.

## Saldeo

Saldeo używa zapisanej sesji webowej Playwright (`~/.config/lab/saldeo-storage-state.json`). Sekrety trzymaj w macOS Keychain; nie zapisuj ich w repo. LAB wysyła do Saldeo tylko lokalne pliki (`source_path`), np. PDF z Gmaila albo XML z eksportu KSeF.

## Uwagi bezpieczeństwa

- Nie zapisuj tokenów, haseł ani kluczy w repo — używaj env var albo systemowego keychain.
- Raporty mogą zawierać NIP-y, numery faktur i ścieżki plików; traktuj `out/` jako dane wrażliwe.
- Ten MVP nie księguje i nie wysyła nic do KSeF; tylko czyta pliki/Gmail i porównuje dane.
